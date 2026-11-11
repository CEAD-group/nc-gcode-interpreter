"""G-group columns are pl.Enum columns whose categories are the group's
commands in manual order, identical in every batch and every run."""

import io

import polars as pl
import pytest
from nc_gcode_interpreter import (
    G_GROUP_DTYPES,
    GGroups,
    NcError,
    nc_to_batches,
    nc_to_dataframe,
    nc_to_rows,
    sanitize_dataframe,
)
from polars.testing import assert_frame_equal

PROGRAM = 'G1 G64 X1\nX2\nG4 F0.5\nG0 X3 T="TOOL"\nG17 X4 ; note\nFOO()\nG2 X5 CR=3\nM30\n'


def _assert_g_group_dtypes(frame: pl.DataFrame) -> None:
    for name, dtype in frame.schema.items():
        if name in G_GROUP_DTYPES:
            assert dtype == G_GROUP_DTYPES[name], name


def test_dtypes_cover_every_group_in_manual_order():
    assert set(G_GROUP_DTYPES) == {g["short_name"] for g in GGroups.g_groups}
    for group in GGroups.g_groups:
        categories = G_GROUP_DTYPES[group["short_name"]].categories.to_list()
        assert categories == [entry["id"] for entry in group["entries"]]
    assert G_GROUP_DTYPES["gg01_motion"].categories.to_list()[:4] == ["G0", "G1", "G2", "G3"]


def test_batches_share_one_enum_dtype_and_concat_vertically():
    frames = list(nc_to_batches(PROGRAM, batch_size=2))
    assert len(frames) > 2
    for frame in frames:
        _assert_g_group_dtypes(frame)
    df = pl.concat(frames, how="diagonal")
    _assert_g_group_dtypes(df)
    assert df["gg01_motion"].to_list() == ["G1", "G1", "G1", "G0", "G0", "G0", "G2", "G2"]
    assert df["gg02_wait"].to_list() == [None, None, "G4", None, None, None, None, None]
    # A separate run with another batch size stacks vertically without a cast.
    other = pl.concat(nc_to_batches(PROGRAM, batch_size=3), how="diagonal").select(df.columns)
    assert pl.concat([df, other]).height == 2 * df.height


def test_other_string_columns_stay_string():
    df, _ = nc_to_dataframe(PROGRAM)
    for name in ("T", "comment", "non_returning_function_call"):
        assert df.schema[name] == pl.String, name


def test_dataframe_equals_concatenated_batches():
    expected, _ = nc_to_dataframe(PROGRAM)
    frames = list(nc_to_batches(PROGRAM, batch_size=2))
    got = pl.concat(frames, how="diagonal").select(frames[-1].columns)
    assert_frame_equal(got, expected, check_exact=True)


def test_disable_forward_fill_keeps_nulls_as_enum_nulls():
    for frames in (
        list(nc_to_batches(PROGRAM, batch_size=2, disable_forward_fill=True)),
        [nc_to_dataframe(PROGRAM, disable_forward_fill=True)[0]],
    ):
        df = pl.concat(frames, how="diagonal")
        _assert_g_group_dtypes(df)
        assert df["gg01_motion"].to_list() == ["G1", None, None, "G0", None, None, "G2", None]


def test_rows_keep_plain_strings():
    rows = [row for _, row in nc_to_rows(PROGRAM)]
    assert rows[0]["gg01_motion"] == "G1"
    assert type(rows[0]["gg01_motion"]) is str


def test_unknown_g_command_is_a_loud_error():
    with pytest.raises(NcError, match="G01") as info:
        nc_to_dataframe("G01 X1\n")
    assert info.value.kind == "unknown_g_command"
    with pytest.raises(NcError, match="G01") as info:
        nc_to_dataframe("X1\n", initial_state="G01\n")
    assert info.value.kind == "unknown_g_command"


def test_literal_comparison_and_manual_order_sort():
    df, _ = nc_to_dataframe(PROGRAM)
    assert df.filter(pl.col("gg01_motion") == "G1").height == 3
    assert df["gg01_motion"].is_in(["G0", "G2"]).sum() == 5
    # Enum sorts by manual order: G0, G1, G2, ... not alphabetically.
    assert df["gg01_motion"].unique().sort().to_list() == ["G0", "G1", "G2"]
    assert df["gg01_motion"].cast(pl.String).dtype == pl.String


def test_parquet_round_trip_keeps_the_dtype():
    df, _ = nc_to_dataframe(PROGRAM)
    buffer = io.BytesIO()
    df.write_parquet(buffer)
    buffer.seek(0)
    assert_frame_equal(pl.read_parquet(buffer), df)


def test_sanitize_dataframe_casts_g_groups_and_rejects_unknown_values():
    clean = sanitize_dataframe(pl.DataFrame({"gg01_motion": ["G1", None], "X": [1.0, 2.0]}))
    assert clean.schema["gg01_motion"] == G_GROUP_DTYPES["gg01_motion"]
    assert clean["gg01_motion"].to_list() == ["G1", "G1"]
    with pytest.raises(pl.exceptions.InvalidOperationError, match="G17"):
        sanitize_dataframe(pl.DataFrame({"gg01_motion": ["G17"], "X": [1.0]}))
