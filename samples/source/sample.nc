; Sample CNC milling toolpath
G21 ; metric
G90 ; absolute positioning
G00 Z5.000 ; rapid lift
G00 X10.000 Y10.000 ; rapid to start
M03 S12000 ; spindle on
G01 Z-1.500 F300 ; plunge
G01 X90.000 Y10.000 F800 ; linear cut
G02 X100.000 Y20.000 I0.0 J10.0 ; clockwise arc corner
G01 X100.000 Y80.000 ; linear cut
G02 X90.000 Y90.000 I-10.0 J0.0 ; clockwise arc corner
G01 X10.000 Y90.000 ; linear cut
G02 X0.000 Y80.000 I0.0 J-10.0 ; clockwise arc corner
G01 X0.000 Y20.000 ; linear cut
G02 X10.000 Y10.000 I10.0 J0.0 ; clockwise arc corner
G00 Z5.000 ; rapid retract
M05 ; spindle off
M02 ; end of program
