- [x] basic math (add, sub, mul, div)
- [x] handle variables (defining, assigning, mutating)
- [x] conditionals (if, else)
- [x] while loops
- [x] for loops
- [x] TRANS statements 
- [x] IC() function
- [x] Error handling
- [x] builtin variables not hardcoded
- [x] functions not hardcoded
- [x] allow supplying custom values for builtin variables
- [x] array syntax
- [] are variables only internal state, or should they be part of output?
- [] is every element in a variable array a separate element/column in the output?
- [x] case (in)sensitivity
- [x] write output while parsing -> no, will not do this
- [] 4.1.3.1 Arithmetic functions
- [] Var  PHU 42 LLI 0 ULI 
- [x] PRESETON(E,0)
- [] TRAILON(ELX,Y,1); set one axis to trail another with a certain offset
- [x] Check if lower case axis names should be matched. Can a variable be named x,y, or z or does that conflict with the axis names? --> indeed it does conflict
- [] Check if EXL10 is a valid variable command or that it should really be ELX=10 (does direct assignment only work for single letter variables?)
- [x] Parse statements like FL[E] = 10 or ACC[E] = 10; (set feedrate for E axis). Here E refers to an index in the array FL, which is a feedrate array, and not the value of the axis. Thus E is both a variable refering to the axis number of E, and simultaneously E is also used to update the axis value.


EXECSTRING


ROT 
ROTS
CROTS X... Y...
SCALE X... Y... Z...
ATRANS X... Y... Z...
AROT X... Y... Z...
AROTS X... Y...
ASCALE X... Y... Z...

AROT RPL=...
ROT RPL=...
MIRROR X0/Y0/Z0
AMIRROR X0/Y0/Z0