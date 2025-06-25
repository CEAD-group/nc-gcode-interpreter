import pathlib
import pytest
from nc_gcode_interpreter import nc_to_dataframe, dataframe_to_nc
import polars as pl
from polars.testing import assert_frame_equal


@pytest.fixture(
    scope="module",
    params=pathlib.Path(__file__)
    .parent.parent.parent.joinpath("examples")
    .glob("*.mpf"),
    ids=lambda param: param.name,  # Add filename as the test ID
)
def mpf_file(request):
    """
    Provide the path of each .mpf file in the examples directory.
    """
    return request.param


@pytest.fixture(scope="module")
def initial_state():
    """
    Load the default state from defaults.mpf file for each test module.
    """
    return (
        pathlib.Path(__file__)
        .parent.parent.parent.joinpath("examples/defaults.mpf")
        .read_text()
    )


def test_mpf_file_to_csv(mpf_file, initial_state):
    """
    Test the nc_to_dataframe function with each .mpf file, using the default initial state.
    """
    
    df, _state = nc_to_dataframe(
        mpf_file.read_text(),
        initial_state=initial_state,
        iteration_limit=10000,
        extra_axes=["ELX"],
        axis_index_map = {"E": 4},
    )
    df_expected = pl.read_csv(mpf_file.with_suffix(".csv"))

    # Drop the "M" column if it exists because the m column is parsed to a list of strings, while the csv file has a single string per row
    if "M" in df_expected.columns:
        # check if any of the values in the M column of df is a list with more than one element
        if (df["M"].map_elements(lambda x: len(x)) > 1).any():
            pytest.skip("M column has more than one element in a row")

        df_expected.drop_in_place("M")
        df.drop_in_place("M")

    assert_frame_equal(df_expected, df, atol=1e-3)


def test_mpf_file_roundtrip(mpf_file, tmp_path, initial_state):
    axis_index_map = {"E": 4} if mpf_file.name == "axis_index_assignment.mpf" else None
    df_expected, _state = nc_to_dataframe(
        mpf_file.read_text(), initial_state=initial_state, axis_index_map=axis_index_map
    )

    tmp_file = tmp_path / mpf_file.name
    dataframe_to_nc(df=df_expected, file_path=tmp_file)

    df, _state = nc_to_dataframe(tmp_file.read_text(), axis_index_map=axis_index_map)

    assert_frame_equal(df_expected, df, atol=1e-3)
