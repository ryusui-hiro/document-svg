"""Offline geometry and template regressions for the business authoring layer."""
import base64
import copy
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET
from authoring import architecture as arch
from authoring.business_bundle import NAMES, template, build

SVG_NS='http://www.w3.org/2000/svg'
ICON=b'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 12"><path d="M0 0h24v12H0z" fill="red"/></svg>'


def resolve(catalog,cache,ident):
    provider,kind,_=ident.split('/',2)
    return {'id':ident,'provider':provider,'kind':kind},'data:image/svg+xml;base64,'+base64.b64encode(ICON).decode()


def minimal():
    return dict(schema_version=2,provider='aws',title='A & B',groups=[],nodes=[
        dict(id='a',label='First',symbol='user',x=60,y=220),
        dict(id='b',label='Second',symbol='external',x=500,y=420),
    ],edges=[dict(id='flow',**{'from':'a','to':'b'},kind='request')])


class ArchitectureTests(unittest.TestCase):
    def render(self,spec):return arch.render(spec,{},Path('.'),resolve)

    def test_all_business_templates_are_deterministic_and_clear(self):
        for name in NAMES:
            with self.subTest(name=name):
                spec=template(name)
                svg,report=self.render(spec)
                self.assertEqual((svg,report),self.render(spec))
                self.assertEqual(report['edge_intersections'],[])
                for key in ('node_overlaps','route_node_collisions','label_collisions','boundary_title_collisions'):
                    self.assertEqual(report['quality'][key],0)
                self.assertLessEqual(report['quality']['maximum_bends'],3)
                root=ET.fromstring(svg)
                self.assertEqual(len(root.findall(f'.//{{{SVG_NS}}}image')),len([n for n in spec['nodes'] if 'icon' in n]))
                for image in root.findall(f'.//{{{SVG_NS}}}image'):
                    self.assertEqual(base64.b64decode(image.get('href').split(',')[1]),ICON)
                    self.assertEqual(image.get('preserveAspectRatio'),'xMidYMid meet')
                for edge in report['routes']:
                    for a,b in zip(edge['points'],edge['points'][1:]):
                        self.assertTrue(a[0]==b[0] or a[1]==b[1])
                    for n in spec['nodes']:
                        if n['id'] not in (edge['source'],edge['target']):
                            r=arch.rect({'width':208,'height':112,**n})
                            self.assertFalse(any(arch.segment_hits(a,b,r) for a,b in zip(edge['points'],edge['points'][1:])))

    def test_facing_nearby_ports_do_not_create_tiny_loops(self):
        spec=minimal();spec['nodes'][1].update(x=304,y=220)
        svg,report=self.render(spec)
        self.assertEqual(report['routes'][0]['points'],[(268.0,276.0),(304.0,276.0)])
        self.assertEqual(report['routes'][0]['bends'],0)

    def test_simplification_keeps_reversals(self):
        self.assertEqual(arch.simplify([(0,0),(10,0),(20,0)]),[(0,0),(20,0)])
        self.assertEqual(arch.simplify([(0,0),(10,0),(5,0)]),[(0,0),(10,0),(5,0)])

    def test_obstacle_router_stays_orthogonal(self):
        obstacle=(80,10,50,80)
        points=arch.route((20,50),(180,50),[obstacle],(0,0,200,120))
        self.assertEqual(points[0],(20,50));self.assertEqual(points[-1],(180,50))
        for a,b in zip(points,points[1:]):
            self.assertFalse(arch.segment_hits(a,b,obstacle))

    def test_unknown_parent_and_group_cycle_are_errors(self):
        spec=template('azure-private-web');spec['groups'][0]['parent']='missing'
        with self.assertRaisesRegex(ValueError,'parent'):self.render(spec)
        spec=template('azure-private-web');spec['groups'][0]['parent']='vnet'
        with self.assertRaisesRegex(ValueError,'cyclic'):self.render(spec)

    def test_group_containment_and_node_ownership(self):
        spec=template('azure-private-web');next(n for n in spec['nodes'] if n['id']=='app')['group']='vnet'
        with self.assertRaisesRegex(ValueError,'outside group'):self.render(spec)
        spec=template('azure-private-web');spec['groups'][1]['width']=3000
        with self.assertRaisesRegex(ValueError,'outside drawing'):self.render(spec)

    def test_overlapping_nodes_are_errors(self):
        spec=minimal();spec['nodes'][1].update(x=65,y=225)
        with self.assertRaisesRegex(ValueError,'overlap'):self.render(spec)

    def test_nonfinite_bool_and_unknown_fields_are_errors(self):
        for value in (float('nan'),float('inf'),True,-1):
            spec=minimal();spec['nodes'][0]['x']=value
            with self.subTest(value=value),self.assertRaises(ValueError):self.render(spec)
        spec=minimal();spec['nodes'][0]['rotation']=90
        with self.assertRaisesRegex(ValueError,'unknown'):self.render(spec)

    def test_bad_endpoints_and_bad_ports_are_errors(self):
        for key,value in [('to','missing'),('from_port','diagonal')]:
            spec=minimal();spec['edges'][0][key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):self.render(spec)

    def test_text_is_escaped_and_overflow_is_rejected(self):
        spec=minimal();spec['nodes'][0]['label']='<A & B>'
        svg,_=self.render(spec)
        self.assertIn('&lt;A &amp; B&gt;',svg)
        spec['nodes'][0]['label']='Very long application name '*3
        with self.assertRaisesRegex(ValueError,'text exceeds'):self.render(spec)

    def test_unsafe_source_link_rejected(self):
        spec=minimal();spec['sources']=[dict(title='Source',url='javascript:alert(1)')]
        with self.assertRaisesRegex(ValueError,'HTTPS'):self.render(spec)

    def test_corner_styles_keep_endpoints_and_scale_arrow_independently(self):
        spec=minimal();spec['edge_style']={'corner':'rounded','radius':8,'jetty':32}
        svg,r=self.render(spec)
        self.assertIn(' Q ',svg)
        self.assertIn('markerUnits="userSpaceOnUse"',svg)
        self.assertIn('refX="10"',svg)
        spec['edge_style']['corner']='sharp'
        sharp,report=self.render(spec)
        self.assertNotIn(' Q ',sharp)
        self.assertEqual(r['routes'],report['routes'])

    def test_corner_fillet_never_cuts_into_obstacles(self):
        points=[(0,30),(30,30),(30,0)]
        path=arch.connector_path(points,8,[(22,22,8,8)])
        self.assertNotIn(' Q ',path)
        self.assertIn(' Q ',arch.connector_path(points,8))
        self.assertTrue(path.startswith('M 0 30'))
        self.assertTrue(path.endswith('L 30 0'))

    def test_reference_templates_preserve_cloud_semantics(self):
        azure=template('azure-private-web')
        self.assertEqual(next(n for n in azure['nodes'] if n['id']=='app')['group'],'paas')
        aws=template('aws-multi-az-web')
        self.assertFalse(any(e['to']=='node_db_b' and e['kind']!='replication' for e in aws['edges']))
        gcp=template('gcp-serverless-web')
        self.assertEqual(next(n for n in gcp['nodes'] if n['id']=='run')['group'],'runtime')
        self.assertEqual(next(g for g in gcp['groups'] if g['id']=='vpc')['kind'],'subnet')

    def test_existing_bundle_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory);sentinel=path/'keep';sentinel.write_text('keep')
            with self.assertRaisesRegex(ValueError,'already exists'):
                build(path,{},path,None)
            self.assertEqual(sentinel.read_text(),'keep')


if __name__=='__main__':unittest.main()
