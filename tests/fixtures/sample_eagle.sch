<?xml version="1.0" encoding="utf-8"?>
<eagle version="9.6.2">
  <drawing>
    <schematic>
      <parts>
        <part name="R1" library="rcl" deviceset="R" device="R" value="1k" />
        <part name="C1" library="rcl" deviceset="C" device="C" value="1u" />
      </parts>
      <sheets>
        <sheet>
          <plain>
            <wire x1="10" y1="20" x2="30" y2="20" layer="91" />
            <wire x1="40" y1="20" x2="60" y2="20" layer="91" />
            <text x="25" y="12" size="1.27" layer="94">EAGLE schematic preview</text>
          </plain>
          <instances>
            <instance part="R1" gate="G$1" x="35" y="20" rot="R0" />
            <instance part="C1" gate="G$1" x="65" y="20" rot="R0" />
          </instances>
          <nets>
            <net name="OUT" class="0">
              <segment>
                <wire x1="30" y1="20" x2="40" y2="20" />
                <label x="35" y="20" size="1.27">OUT</label>
              </segment>
            </net>
          </nets>
        </sheet>
      </sheets>
    </schematic>
  </drawing>
</eagle>
