import pathlib
import pytest
from nc_gcode_interpreter import nc_to_dataframe
from polars import DataFrame


@pytest.fixture(
    scope="module",
    params=pathlib.Path(__file__)
    .parent.parent.parent.joinpath("examples")
    .glob("*.mpf"),
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
    df: DataFrame = nc_to_dataframe(
        mpf_file.read_text(),
        initial_state=initial_state,
        iteration_limit=10000,
        extra_axes=["ELX"],
    )
