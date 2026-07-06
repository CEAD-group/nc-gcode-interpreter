# NC-GCode-Interpreter

A robust interpreter designed for processing Sinumerik-flavored NC G-code, capable of converting MPF files into CSV outputs or Polars DataFrames via Python bindings.

## Overview

The **NC-GCode-Interpreter** offers a streamlined and efficient solution for interpreting G-code specifically tailored to Sinumerik specifications. This tool caters to both command-line interface (CLI) users and those preferring a Python environment, ensuring versatility and ease of use in processing NC programming commands into structured formats like CSV files or Polars DataFrames.

## Features

### Supported G-code Features

- **G Group Commands**: Recognizes G-code groups and modal G-code commands.
- **Global Transformations**: Supports commands like `TRANS` and `ATRANS` for adjusting coordinates globally.
- **Looping Constructs**: Handles loops using `WHILE`, `FOR`, `REPEAT ... UNTIL` and `LOOP ... ENDLOOP` statements.
- **Variable Handling**: Supports definition and manipulation of local variables.
- **Conditional Logic**: Implements conditional execution with `IF`, `ELSE`, and `ENDIF`.
- **Program Jumps**: Jump labels (`MY_LABEL:`) and block numbers as jump destinations for `GOTOF`, `GOTOB`, `GOTO` and `GOTOC`, including single-block conditional jumps (`IF R4>0 GOTOB LA1`) and the `CASE ... OF ... DEFAULT ...` program branch. An executed `M2`/`M17`/`M30` ends the program, so code after the end marker (common in programs with jumps) is not executed. `GOTOS` is parsed but continues with the next block, matching the control's behavior when the PLC does not request a program restart.
- **Arithmetic Operations**: Supports basic operations such as addition, subtraction, multiplication, and division, plus the arithmetic functions `SIN`, `COS`, `TAN`, `ASIN`, `ACOS`, `ATAN2`, `SQRT`, `ABS`, `POT`, `TRUNC`, `ROUND`, `ROUNDUP`, `LN`, `EXP`, `MINVAL`, `MAXVAL` and `BOUND`.
- **Array Operations**: Manages arrays and allows operations on them.
- **Incremental Changes**: Facilitates incremental changes in axes positions like `X=IC(2)`.
- **Spline Programming**: `ASPLINE`, `BSPLINE` and `CSPLINE` blocks with their start/end conditions (`BAUTO`/`BNAT`/`BTAN`, `EAUTO`/`ENAT`/`ETAN`) and the spline block addresses `PW` (point weight), `SD` (spline degree) and `PL` (parameter interval length). Block addresses appear as output columns; unlike axes they receive no `TRANS` offset and are not forward-filled, since e.g. a point weight only applies to the point it is programmed with.

### Additional Functionality

- **Streaming interpretation**: `nc_to_rows(program)` yields `(line_no, row)` tuples lazily while the interpreter runs on a background thread — constant memory, early exit by dropping the iterator (breaking out of a `for` loop over an anonymous iterator does this; a stored iterator keeps the run alive until it is deleted or garbage-collected), and source-line numbers for mapping trace rows back to the program. With `include_variables=True` it yields `(line_no, row, variables)` instead, exposing every variable assignment (`R1=R1+1`, `DEF`, FOR counters) as it happens — including blocks that only assign variables, which are invisible in the batch DataFrame.

- **Custom Axes**: Allows users to define additional axes beyond the standard `X`, `Y`, `Z`.
- **Initial State Configuration**: Enables the use of an initial state MPF file to set default values for multiple runs.
- **CLI Options**: Numerous command-line options to customize the processing, such as axis overriding, loop limits, and more.

## Example Usage

Consider this example program to generate a square in two layers:

```scheme
; Example.MPF
DEF INT n_layers = 2, layer = 1
DEF REAL size = 100 ; size of the square
DEF REAL layer_height = 4 ; height of each layer
TRANS Z = 0.5 ; move up all Z coordinates by 0.5
G1 F=1000 ; Set feed rate in millimeters per minute
G1 X0 Y500 Z0 ; move to the starting point
WHILE (layer <= n_layers)
    X=IC(size)
    Y=IC(size)
    X=IC(-size)
    Y=IC(-size) Z=IC(layer_height)
    layer = layer + 1
ENDWHILE
M31 ; end of program


### CLI Usage
```bash
$ cargo run -- --help
A G-code interpreter

Usage: nc-gcode-interpreter [OPTIONS] <input>

Arguments:
  <input>  Input G-code file (.mpf)

Options:
  -a, --axes <AXIS>                    Override default axis identifiers (comma-separated, e.g., "X,Y,Z")
  -e, --extra-axes <EXTRA_AXIS>        Add extra axis identifiers (comma-separated, e.g., "RA1,RA2")
  -i, --initial_state <INITIAL_STATE>  Optional initial state file to e.g. define global variables or set axis positions
  -l, --iteration_limit <LIMIT>        Maximum number of iterations for loops [default: 10000]
  -f, --disable-forward-fill           Disable forward-filling of null values in axes columns
  -h, --help                           Print help
  -V, --version                        Print version

$ cargo run -- Example.MPF
```

```csv
X			,Y			,Z			,F			,M	 ,gg01_motion	,comment
			,			,			,			,	 ,			    ,;size of the square
			,			,			,			,	 ,			    ,;size of the square
			,			,			,			,	 ,			    ,; move up all z coordinates by 0.5
			,			,			,1000.000	,	 ,G1			,; Set feed rate in millimeters per minute
0.000		,500.000	,0.500		,1000.000	,	 ,G1			,; move to the starting point
100.000		,500.000	,0.500		,1000.000	,	 ,G1			,
100.000		,600.000	,0.500		,1000.000	,	 ,G1			,
0.000		,600.000	,0.500		,1000.000	,	 ,G1			,
0.000		,500.000	,5.000		,1000.000	,	 ,G1			,
100.000		,500.000	,5.000		,1000.000	,	 ,G1			,
100.000		,600.000	,5.000		,1000.000	,	 ,G1			,
0.000		,600.000	,5.000		,1000.000	,	 ,G1			,
0.000		,500.000	,9.500		,1000.000	,	 ,G1			,
0.000		,500.000	,9.500		,1000.000	,M31 ,G1			,; end of program
```


### python example

To install the Python bindings, run:
```bash
pip install nc-gcode-interpreter
```

Then, you can use the Python bindings to convert an MPF file to a DataFrame:

```bash
python -c "\
from nc_gcode_interpreter import nc_to_dataframe; \
from pathlib import Path; \
df, state = nc_to_dataframe(Path('Example.MPF').open()); \
print(df)"
shape: (14, 7)
┌───────┬───────┬──────┬────────┬───────────┬─────────────┬─────────────────────────────────┐
│ X     ┆ Y     ┆ Z    ┆ F      ┆ M         ┆ gg01_motion ┆ comment                         │
│ ---   ┆ ---   ┆ ---  ┆ ---    ┆ ---       ┆ ---         ┆ ---                             │
│ f64   ┆ f64   ┆ f64  ┆ f64    ┆ list[str] ┆ str         ┆ str                             │
╞═══════╪═══════╪══════╪════════╪═══════════╪═════════════╪═════════════════════════════════╡
│ null  ┆ null  ┆ null ┆ null   ┆ null      ┆ null        ┆ ;size of the square             │
│ null  ┆ null  ┆ null ┆ null   ┆ null      ┆ null        ┆ ;size of the square             │
│ null  ┆ null  ┆ null ┆ null   ┆ null      ┆ null        ┆ ; move up all z coordinates by… │
│ null  ┆ null  ┆ null ┆ 1000.0 ┆ null      ┆ G1          ┆ ; Set feed rate in millimeters… │
│ 0.0   ┆ 500.0 ┆ 0.5  ┆ 1000.0 ┆ null      ┆ G1          ┆ ; move to the starting point    │
│ …     ┆ …     ┆ …    ┆ …      ┆ …         ┆ …           ┆ …                               │
│ 100.0 ┆ 500.0 ┆ 5.0  ┆ 1000.0 ┆ null      ┆ G1          ┆ null                            │
│ 100.0 ┆ 600.0 ┆ 5.0  ┆ 1000.0 ┆ null      ┆ G1          ┆ null                            │
│ 0.0   ┆ 600.0 ┆ 5.0  ┆ 1000.0 ┆ null      ┆ G1          ┆ null                            │
│ 0.0   ┆ 500.0 ┆ 9.5  ┆ 1000.0 ┆ null      ┆ G1          ┆ null                            │
│ 0.0   ┆ 500.0 ┆ 9.5  ┆ 1000.0 ┆ ["M31"]   ┆ G1          ┆ ; end of program                │
└───────┴───────┴──────┴────────┴───────────┴─────────────┴─────────────────────────────────┘
```

The Python bindings also return the state of the program after execution, which can be used for inspection.

#### Streaming

For long programs (or when you want results before the whole file is interpreted), `nc_to_rows` yields rows lazily while the interpreter runs on a background thread. Each row carries the 1-based source line it came from — loops and jumps repeat line numbers, which is exactly what a visualizer needs to map trace rows back to the program:

```python
from nc_gcode_interpreter import nc_to_rows

program = "R1=0\nWHILE R1<3\nG1 X=R1*10 F1000\nR1=R1+1\nENDWHILE\nM30"

for line_no, row in nc_to_rows(program):
    print(line_no, row["X"])
# 3 0.0
# 3 10.0
# 3 20.0
# 6 20.0

# With include_variables=True, every variable assignment streams as a
# per-row delta - including variable-only blocks, which are invisible in
# the batch DataFrame. Accumulating the deltas with dict.update
# reconstructs the full variable state at any point of the stream.
for line_no, row, variables in nc_to_rows(program, include_variables=True):
    print(line_no, row.get("X"), variables)
# 1 None {'R1': 0.0}
# 3 0.0 {}
# 4 None {'R1': 1.0}
# ...
```

Rows are typed and forward-filled like the batch DataFrame (disable with `forward_fill=False`); errors raise from `next()` at the offending row; after exhaustion the iterator's `state` attribute holds the final interpreter state. See `python/example/streaming.py` for a runnable version.

Additionally, conversion from a Polars DataFrame back to an MPF (NC) program is also supported:

```bash
python -c "\
from nc_gcode_interpreter import nc_to_dataframe, dataframe_to_nc; \
from pathlib import Path; \
df, state = nc_to_dataframe(Path('Example.MPF').open(), extra_axes=['ELX']); \
dataframe_to_nc(df, Path('Example_out.MPF').open('w'))" 
```

```bash
target/release/nc-gcode-interpreter --help
A G-code interpreter

Usage: nc-gcode-interpreter [OPTIONS] <input>

Arguments:
  <input>  Input G-code file (.mpf)

Options:
  -a, --axes <AXIS>                    Override default axis identifiers (comma-separated, e.g., "X,Y,Z")
  -e, --extra-axes <EXTRA_AXIS>        Add extra axis identifiers (comma-separated, e.g., "RA1,RA2")
  -i, --initial_state <INITIAL_STATE>  Optional initial_state file to initialize state
  -l, --iteration_limit <LIMIT>             Maximum number of iterations for loops [default: 10000]
  -f, --disable-forward-fill           Disable forward-filling of null values in axes columns
  -h, --help                           Print help
  -V, --version                        Print version
```

## Why?

The Sinumerik NC programming guide is extensive, and some of its functionality can be very convenient for making on-the-fly improvements to code. However, to better understand, visualize, and simulate the code, it is often necessary to convert it to a more structured format like CSV or a DataFrame. This tool aims to provide a simple and efficient way to convert Sinumerik-flavored G-code to a structured format, making it easier to analyze and visualize.

Only a limited subset is supported, but the tool is designed to be easily extensible to support more features in the future.

## Contributing
We welcome contributions! Please see our [Contributing Guidelines](CONTRIBUTING.md) for more details on how to get started.
