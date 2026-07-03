# %%
from nc_gcode_interpreter import nc_to_rows

program = """
DEF REAL depth=2.5
R1=0
WHILE R1<3
G1 X=R1*10 Z=-depth F1000
R1=R1+1
ENDWHILE
M30
"""

# Rows stream while the interpreter runs on a background thread: constant
# memory, and each row carries the source line it came from (loops repeat
# line numbers - exactly what a visualizer needs for trace-to-source
# highlighting).
for line_no, row in nc_to_rows(program):
    print(f"line {line_no}: X={row.get('X')} Z={row.get('Z')}")

# %%
# include_variables=True also streams every variable assignment as a
# per-row delta - including blocks that only assign variables, which are
# invisible in the batch DataFrame. Accumulating the deltas reconstructs
# the full variable state at any point of the stream.
variables = {}
for line_no, row, changes in nc_to_rows(program, include_variables=True):
    variables.update(changes)
    print(f"line {line_no}: {row or '(no output)'} variables={variables}")

# %%
