from nc_gcode_interpreter import GGroups


def test_g_groups_class() -> None:
    g = GGroups
    assert g.is_g_group("gg01_motion")
    assert not g.is_g_group("G1")


def test_g_groups_instance() -> None:
    g = GGroups()
    assert g.is_g_group("gg01_motion")
    assert not g.is_g_group("G1")


def test_g_groups_access_class() -> None:
    g: list = GGroups.g_groups
    assert isinstance(g, list)


def test_g_groups_access_instance() -> None:
    g: list = GGroups().g_groups
    assert isinstance(g, list)


def _numbers(short_name: str) -> dict[str, int]:
    group = next(g for g in GGroups.g_groups if g["short_name"] == short_name)
    return {entry["id"]: entry["nr"] for entry in group["entries"]}


def test_group_numbers_follow_the_manual() -> None:
    """Group numbers and command numbers as in the manual's G-group tables (#120)."""
    for group in GGroups.g_groups:
        assert int(group["short_name"][2:4]) == group["nr"]
        assert group["title"].startswith(f"G group {group['nr']}:")
        numbers = [entry["nr"] for entry in group["entries"]]
        assert len(numbers) == len(set(numbers)), group["short_name"]
    dynamic = _numbers("gg59_dyn_resp_path")
    assert dynamic == {"DYNNORM": 1, "DYNPOS": 2, "DYNROUGH": 3, "DYNSEMIFIN": 4, "DYNFINISH": 5, "DYNPREC": 6}
    assert _numbers("gg60_area_limit")["WALCS0"] == 1
    assert _numbers("gg53_frame_rot_tool")["TOROTOF"] == 1
    assert _numbers("gg56_tool_wear")["TOWSTD"] == 1
    assert _numbers("gg38_punch_fast_io")["SPIF1"] == 1
    grinding = _numbers("gg64_grinding_frames")
    assert grinding["GFRAME[0]"] == 1 and grinding["GFRAME[100]"] == 101
    assert not GGroups.is_g_group("gg58_dyn_resp_path")


def test_renamed_and_added_commands_reach_their_columns() -> None:
    from nc_gcode_interpreter import nc_to_dataframe

    program = "DYNFINISH\nWALCS1\nTOROTOF\nTOWWCS\nG646\nCOMPPATH\nORIAXESFR\nCUT3DCP\nSPIF2\nG1 X1 F100\n"
    df, _ = nc_to_dataframe(program)
    last = df.row(-1, named=True)
    assert last["gg59_dyn_resp_path"] == "DYNFINISH"
    assert last["gg60_area_limit"] == "WALCS1"
    assert last["gg53_frame_rot_tool"] == "TOROTOF"
    assert last["gg56_tool_wear"] == "TOWWCS"
    assert last["gg10_exact_stop_mode"] == "G646"
    assert last["gg30_nc_compress"] == "COMPPATH"
    assert last["gg51_interp_type_orient"] == "ORIAXESFR"
    assert last["gg22_tool_offset_type"] == "CUT3DCP"
    assert last["gg38_punch_fast_io"] == "SPIF2"
    assert "non_returning_function_call" not in df.columns
