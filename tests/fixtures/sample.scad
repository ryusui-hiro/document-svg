// Safe OpenSCAD structure fixture
$fn = 32;
module rounded_box(size=[20,10,5]) {
  difference() {
    cube(size, center=true);
    translate([0,0,2]) sphere(r=3);
  }
}
function safe_radius(value) = value / 2;
use <private-library.scad>;
include <private-config.scad>;
import("private-model.stl");
translate([0,0,5]) rotate([0,0,15]) rounded_box();
