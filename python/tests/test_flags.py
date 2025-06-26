import pytest
from nc_gcode_interpreter import nc_to_dataframe


def test_undefined_variables(capsys):
    """
    Test that undefined variables are handled correctly based on allow_undefined_variables setting.
    """
    # Test with allow_undefined_variables=False (default)
    with pytest.raises(Exception):
        nc_to_dataframe("G1 X=AA")  # AA is undefined
    
    # Test with allow_undefined_variables=True
    df, state = nc_to_dataframe("G1 X=AA", allow_undefined_variables=True)

    assert df["X"][0] == 0.0  # Undefined variable AA should be treated as 0
    assert "AA" in state["symbol_table"]  # Verify AA is added to the symbol table
    assert state["symbol_table"]["AA"] == 0.0  # Verify AA is set to 0.0
