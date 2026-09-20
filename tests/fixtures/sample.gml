<?xml version="1.0" encoding="UTF-8"?>
<gml:FeatureCollection xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="https://example.test/features" xmlns:xlink="http://www.w3.org/1999/xlink" srsName="urn:ogc:def:crs:OGC::CRS84">
  <gml:featureMember>
    <app:Road gml:id="road-1">
      <app:name>Coastal route</app:name>
      <app:shape>
        <gml:LineString srsDimension="2">
          <gml:posList count="3">-122.09 37.41 -122.08 37.42 -122.07 37.415</gml:posList>
        </gml:LineString>
      </app:shape>
    </app:Road>
  </gml:featureMember>
  <gml:featureMember>
    <app:Park>
      <app:shape>
        <gml:Polygon>
          <gml:exterior><gml:LinearRing><gml:posList>-122.10 37.40 -122.06 37.40 -122.06 37.44 -122.10 37.44 -122.10 37.40</gml:posList></gml:LinearRing></gml:exterior>
          <gml:interior><gml:LinearRing><gml:posList>-122.09 37.41 -122.08 37.41 -122.08 37.42 -122.09 37.42 -122.09 37.41</gml:posList></gml:LinearRing></gml:interior>
        </gml:Polygon>
      </app:shape>
    </app:Park>
  </gml:featureMember>
  <gml:featureMember>
    <app:Location>
      <app:shape>
        <gml:Point srsName="urn:ogc:def:crs:EPSG::4326"><gml:pos srsDimension="3">37.4222899 -122.0822035 12.5</gml:pos></gml:Point>
      </app:shape>
    </app:Location>
  </gml:featureMember>
  <gml:featureMember><app:Linked xlink:href="https://example.invalid/remote.gml"/></gml:featureMember>
</gml:FeatureCollection>
