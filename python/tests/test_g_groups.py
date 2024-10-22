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
