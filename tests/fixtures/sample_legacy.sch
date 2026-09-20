EESchema Schematic File Version 4
LIBS:power
EELAYER 29 0
EELAYER END
$Descr A4 11693 8268
Sheet 1 1
Title "Legacy preview"
Date "2026-09-16"
Rev "1"
Comp "document-svg"
Comment1 "Safe fixture"
$EndDescr
$Comp
L Device:R R1
U 1 1 5F000001
P 4000 3000
F 0 "R1" V 3793 3000 50  0000 C CNN
F 1 "1k" V 3884 3000 50  0000 C CNN
F 2 "" V 3930 3000 50  0001 C CNN
F 3 "" H 4000 3000 50  0001 C CNN
	1    4000 3000
	0    1    1    0
$EndComp
$Comp
L Device:C C1
U 1 1 5F000002
P 5500 3000
F 0 "C1" V 5248 3000 50  0000 C CNN
F 1 "1u" V 5339 3000 50  0000 C CNN
F 2 "" V 5430 3000 50  0001 C CNN
F 3 "" H 5500 3000 50  0001 C CNN
	1    5500 3000
	0    1    1    0
$EndComp
Wire Wire Line
	4150 3000 5350 3000
Wire Wire Line
	3850 3000 3500 3000
Text Label 4750 3000 0    50   ~ 0
FILTERED_OUT
Text Notes 4500 2500 0    60   ~ 12
Legacy Eeschema preview
Connection ~ 4750 3000
$EndSCHEMATC
