# Graph Modeling Language sample
graph [
  directed 1
  label "Order workflow"
  node [ id 1 label "Start order" graphics [ x 10 y 20 ] ]
  node [ id 2 label "Review" ]
  node [ id 3 label "Complete" ]
  edge [ source 1 target 2 label "submit" ]
  edge [ source 2 target 3 label "approve" ]
]
