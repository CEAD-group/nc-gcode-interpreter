"""Sinumerik REAL comparison semantics.

REAL comparisons use relative equality with a tolerance of 1e-12 rather than
absolute equality (NC programming manual, "Precision correction on comparison
errors (TRUNC)"). This applies to ==, <>, <= and >=, and by default also
excludes relatively-equal values from < and >.
"""

import pytest
from nc_gcode_interpreter import nc_to_dataframe


def run_condition(condition: str) -> bool:
    """Evaluate a relational condition and return its truth value."""
    program = f"""DEF REAL res=0
IF {condition}
res=1
ENDIF
X=res
M30
"""
    df, _state = nc_to_dataframe(program)
    return df["X"].to_list()[-1] == 1.0


@pytest.mark.parametrize(
    "condition, expected",
    [
        # 0.1+0.2 differs from 0.3 by ~4.4e-17: relatively equal
        ("(0.1+0.2)==0.3", True),
        ("(0.1+0.2)<>0.3", False),
        ("(0.1+0.2)<=0.3", True),
        ("(0.1+0.2)>=0.3", True),
        # relative equality also excludes > and < (MD10280 Bit0=0 default)
        ("(0.1+0.2)>0.3", False),
        ("(0.1+0.2)<0.3", False),
        # genuinely different values still compare as expected
        ("0.31>0.3", True),
        ("0.29<0.3", True),
        ("0.31==0.3", False),
        ("0.31<>0.3", True),
        # exact values
        ("0.5==0.5", True),
        ("0.5<=0.5", True),
        ("0.5>=0.5", True),
        ("0.5<0.5", False),
        ("0.5>0.5", False),
    ],
)
def test_real_comparison(condition, expected):
    assert run_condition(condition) == expected


def test_large_block_number_preserved():
    """N is kept as its integer lexeme, not round-tripped through a float."""
    df, _state = nc_to_dataframe("N12345678901234 G1 X1")
    assert df["N"].to_list() == [12345678901234]
