# %%
from nc_gcode_interpreter import nc_to_dataframe

input_data = """
DEF int aa=1;
G1 ;  Adsds
G2 X3 Z100
G3 X1 X=3 Y4.34 
; sdf

G341
"""

df, state = nc_to_dataframe(input_data, iteration_limit=10000, extra_axes=["ELX"])
print(df)
print(state)
# %%
