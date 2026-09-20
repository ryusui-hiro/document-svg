Document Conversion Notes
=========================

Overview
--------

reStructuredText headings use adornment lines. This paragraph includes **strong**
text, *emphasis*, ``inline code``, a :ref:`named target`, and a `visible link <https://example.invalid>`_.

Features
~~~~~~~~

* Multiple section levels
* Numbered steps

#. Read the page
#. Render the SVG

Literal example::

    print("markup is not evaluated")

.. note:: Safe preview

   Directives that could read files or insert raw output stay inactive.

Simple table
------------

==============  ===========
Component       Status
==============  ===========
Parser          Ready
Renderer        Ready
==============  ===========

Grid table
----------

+-----------+----------------+
| Format    | Preview        |
+===========+================+
| reST      | Sectioned text |
+-----------+----------------+

Local figure
------------

.. figure:: assets/red-blue.png
   :alt: RST red and blue image

   Figure caption is retained below the image.

.. image:: https://example.invalid/not-fetched.png
   :alt: remote image

.. include:: must-not-be-read.txt

.. raw:: html

   <script>must-not-run()</script>
