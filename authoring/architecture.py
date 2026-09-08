"""Business architecture diagrams: nested boundaries, orthogonal routes, review metadata.

No cloud API calls. Layout/architecture are explicitly authored; route finding is deterministic.
"""
from __future__ import annotations

import heapq
import html
import json
import math
import re
import unicodedata
from urllib.parse import urlparse

THEMES = {'azure': ('#0078d4', 'MICROSOFT AZURE'), 'aws': ('#b85d06', 'AMAZON WEB SERVICES'),
          'gcp': ('#18845b', 'GOOGLE CLOUD'), 'neutral': ('#365878', 'CLOUD ARCHITECTURE')}
FLOW = {'request': ('#2463a6', '', 'リクエスト'), 'private': ('#087f82', '', 'プライベート通信'),
        'replication': ('#8256a7', '7 5', '同期 / 複製'), 'telemetry': ('#697587', '3 5', '監視 / ログ'),
        'control': ('#aa6808', '8 4', '制御 / 設定')}
GROUPS = {'cloud', 'region', 'vpc', 'subnet', 'zone', 'service', 'logical'}
PORTS = {'n': (0, -1), 'e': (1, 0), 's': (0, 1), 'w': (-1, 0)}


def esc(value):
    return html.escape(str(value), quote=True)


def checked_text(value, name, limit=160, empty=False):
    if not isinstance(value, str) or len(value) > limit or (not empty and not value.strip()):
        raise ValueError(f'{name}: expected text of 1..{limit} characters')
    if any(ord(c) < 32 and c != '\n' for c in value):
        raise ValueError(f'{name}: invalid control character')
    return value


def fields(obj, allowed, required, name):
    if not isinstance(obj, dict) or set(obj) - set(allowed) or set(required) - set(obj):
        raise ValueError(f'{name}: unknown or missing fields; allowed: {", ".join(allowed)}')


def number(value, name, lo=0, hi=4000):
    if type(value) not in (int, float) or not math.isfinite(value) or not lo <= value <= hi:
        raise ValueError(f'{name}: finite number in {lo}..{hi} required')
    return float(value)


def ident(value):
    if not isinstance(value, str) or not re.fullmatch('[A-Za-z][A-Za-z0-9_-]{0,63}', value):
        raise ValueError('IDs must start with a letter and contain at most 64 ASCII letters, digits, _ or -')
    return value


def rect(obj):
    return tuple(number(obj[k], k) for k in ('x', 'y', 'width', 'height'))


def expand(r, margin):
    x, y, w, h = r
    return x-margin, y-margin, w+2*margin, h+2*margin


def overlaps(a, b):
    return a[0] < b[0]+b[2] and b[0] < a[0]+a[2] and a[1] < b[1]+b[3] and b[1] < a[1]+a[3]


def inside(inner, outer):
    x, y, w, h = inner
    a, b, c, d = outer
    return x >= a and y >= b and x+w <= a+c and y+h <= b+d


def segment_hits(a, b, r):
    x, y, w, h = r
    if a[0] == b[0]:
        return x < a[0] < x+w and max(a[1], b[1]) > y and min(a[1], b[1]) < y+h
    if a[1] == b[1]:
        return y < a[1] < y+h and max(a[0], b[0]) > x and min(a[0], b[0]) < x+w
    raise ValueError('route segments must be orthogonal')


def text_width(value, size):
    # Conservative deterministic estimate, independent of installed font metrics.
    return sum(size if unicodedata.east_asian_width(c) in 'WF' else
               size * (0.36 if c in ' ilI.,:;!|\'' else 0.9 if c in 'MW@' else 0.61) for c in value)


def lines(value, width, size, maximum, name):
    result = []
    for paragraph in value.split('\n'):
        current = ''
        for token in re.findall(r'[A-Za-z0-9_/.-]+\s*|.', paragraph):
            if text_width(current + token, size) <= width:
                current += token
                continue
            if current.strip():
                result.append(current.rstrip())
                current = ''
            for c in token.lstrip():
                if text_width(current+c, size) > width:
                    result.append(current)
                    current = ''
                current += c
        result.append(current.rstrip())
    if len(result) > maximum:
        raise ValueError(f'{name}: text exceeds available space ({maximum} lines)')
    return result


def svg_text(value, x, y, size=14, color='#20344b', weight=400, width=300, maximum=1, name='label'):
    wrapped = lines(value, width, size, maximum, name)
    return ''.join(f'<text x="{x:g}" y="{y+i*size*1.45:g}" font-size="{size}" font-weight="{weight}" fill="{color}">{esc(line)}</text>' for i, line in enumerate(wrapped))


def port(r, side, offset=0):
    x, y, w, h = r
    return {'n': (x+w*(.5+offset), y), 's': (x+w*(.5+offset), y+h),
            'e': (x+w, y+h*(.5+offset)), 'w': (x, y+h*(.5+offset))}[side]


def simplify(points):
    out = []
    for p in points:
        if out and p == out[-1]:
            continue
        if len(out) >= 2 and ((out[-2][0] == out[-1][0] == p[0]) or (out[-2][1] == out[-1][1] == p[1])) and (
            (out[-1][0]-out[-2][0])*(p[0]-out[-1][0]) + (out[-1][1]-out[-2][1])*(p[1]-out[-1][1]) > 0):
            out[-1] = p
        else:
            out.append(p)
    return out


def route(start, end, obstacles, bounds, occupied=(), initial=-1, terminal=-1):
    """Sparse rectilinear visibility graph + A*, with turn and existing-route penalties."""
    if start == end:
        return [start]
    x0, y0, w, h = bounds
    if any(not inside((p[0], p[1], 0, 0), bounds) for p in (start, end)):
        raise ValueError('route endpoint outside drawing area')
    if any(segment_hits(start, start, r) or segment_hits(end, end, r) for r in obstacles):
        raise ValueError('route waypoint/port is inside an obstacle')
    xs = {x0, x0+w, start[0], end[0]}
    ys = {y0, y0+h, start[1], end[1]}
    for x, y, rw, rh in obstacles:
        xs.update(v for v in (x, x+rw) if x0 <= v <= x0+w)
        ys.update(v for v in (y, y+rh) if y0 <= v <= y0+h)
    xs, ys = sorted(xs), sorted(ys)
    source, target = (xs.index(start[0]), ys.index(start[1])), (xs.index(end[0]), ys.index(end[1]))
    queue = [(0, 0, source[0], source[1], initial)]
    costs = {(source[0], source[1], initial): 0}
    parents, clear = {}, {}
    expanded = 0
    while queue:
        _, cost, ix, iy, direction = heapq.heappop(queue)
        state = ix, iy, direction
        if cost != costs.get(state):
            continue
        if (ix, iy) == target and (terminal == -1 or direction != (terminal+2)%4):
            points = [(xs[ix], ys[iy])]
            while state in parents:
                state = parents[state]
                points.append((xs[state[0]], ys[state[1]]))
            return simplify(list(reversed(points)))
        expanded += 1
        if expanded > 100000:
            raise ValueError('route search limit exceeded; simplify diagram or specify via points')
        for dx, dy, nd in ((1, 0, 0), (-1, 0, 2), (0, 1, 1), (0, -1, 3)):
            if direction != -1 and nd == (direction+2)%4:
                continue
            nx, ny = ix+dx, iy+dy
            if not (0 <= nx < len(xs) and 0 <= ny < len(ys)):
                continue
            a, b = (xs[ix], ys[iy]), (xs[nx], ys[ny])
            key = tuple(sorted((a, b)))
            if key not in clear:
                clear[key] = not any(segment_hits(a, b, r) for r in obstacles)
            if not clear[key]:
                continue
            length = abs(a[0]-b[0]) + abs(a[1]-b[1])
            penalty = sum(45 for c, d in occupied if segments_intersect(a, b, c, d))
            next_cost = cost + length + (40 if direction not in (-1, nd) else 0) + penalty
            ns = nx, ny, nd
            if next_cost >= costs.get(ns, math.inf):
                continue
            costs[ns], parents[ns] = next_cost, (ix, iy, direction)
            distance = abs(b[0]-end[0]) + abs(b[1]-end[1])
            heapq.heappush(queue, (next_cost+distance, next_cost, nx, ny, nd))
    raise ValueError('no unobstructed route; move nodes or boundaries to leave a routing corridor')


def segments_intersect(a, b, c, d):
    if a[0] == b[0] and c[1] == d[1]:
        return min(c[0], d[0]) < a[0] < max(c[0], d[0]) and min(a[1], b[1]) < c[1] < max(a[1], b[1])
    if a[1] == b[1] and c[0] == d[0]:
        return segments_intersect(c, d, a, b)
    if a[0] == b[0] == c[0] == d[0]:
        return min(max(a[1], b[1]), max(c[1], d[1])) > max(min(a[1], b[1]), min(c[1], d[1]))
    if a[1] == b[1] == c[1] == d[1]:
        return min(max(a[0], b[0]), max(c[0], d[0])) > max(min(a[0], b[0]), min(c[0], d[0]))
    return False


def connector_path(points, radius=0, obstacles=()):
    """Sharp orthogonal line, or local quadratic corner fillets; endpoints stay exact."""
    points=simplify(points)
    commands=[f'M {points[0][0]:g} {points[0][1]:g}']
    for a,b,c in zip(points,points[1:],points[2:]):
        first=math.dist(a,b);second=math.dist(b,c)
        amount=min(radius,first/2,second/2)
        before=after=b
        while amount>=1:
            before=(b[0]+(a[0]-b[0])*amount/first,b[1]+(a[1]-b[1])*amount/first)
            after=(b[0]+(c[0]-b[0])*amount/second,b[1]+(c[1]-b[1])*amount/second)
            bounds=(min(before[0],after[0],b[0]),min(before[1],after[1],b[1]),abs(before[0]-after[0]),abs(before[1]-after[1]))
            if not any(overlaps(bounds,r) for r in obstacles):
                break
            amount/=2
        if amount>=1:
            commands += [f'L {before[0]:g} {before[1]:g}',f'Q {b[0]:g} {b[1]:g} {after[0]:g} {after[1]:g}']
        else:
            commands.append(f'L {b[0]:g} {b[1]:g}')
    commands.append(f'L {points[-1][0]:g} {points[-1][1]:g}')
    return ' '.join(commands)


def render(spec, catalog, cache, resolve):
    fields(spec, ['schema_version','title','subtitle','provider','canvas','groups','nodes','edges','notes','sources','revision','edge_style'],
           ['schema_version','title','provider','groups','nodes','edges'], 'diagram')
    if spec['schema_version'] != 2 or spec['provider'] not in THEMES:
        raise ValueError('schema_version must be 2; provider must be azure, aws, gcp, or neutral')
    accent, provider = THEMES[spec['provider']]
    style=spec.get('edge_style',{})
    fields(style,['corner','radius','width','jetty'],[],'edge_style')
    corner=style.get('corner','sharp')
    if corner not in ('sharp','rounded'):
        raise ValueError('edge corner must be sharp or rounded')
    radius=number(style.get('radius',8),'corner radius',0,12)
    line_width=number(style.get('width',1.65),'line width',1,3)
    jetty=number(style.get('jetty',32),'jetty',12,64)
    canvas = spec.get('canvas', {'width': 1760, 'height': 1120})
    fields(canvas, ['width','height'], ['width','height'], 'canvas')
    width = number(canvas['width'], 'canvas width', 1200, 3200)
    height = number(canvas['height'], 'canvas height', 800, 2400)
    drawing = (40, 160, width-410, height-310)
    title = checked_text(spec['title'], 'title', 100)
    subtitle = checked_text(spec.get('subtitle', ''), 'subtitle', 180, True)
    revision = checked_text(spec.get('revision', 'REFERENCE DESIGN'), 'revision', 60)
    groups, nodes, edges = spec['groups'], spec['nodes'], spec['edges']
    for values, low, high, name in ((groups,0,40,'groups'), (nodes,1,64,'nodes'), (edges,0,96,'edges')):
        if not isinstance(values,list) or not low <= len(values) <= high:
            raise ValueError(f'{name}: expected {low}..{high} items')
    group_map, node_map, rects, ancestors, headers = {}, {}, {}, {}, []
    for group in groups:
        fields(group,['id','label','kind','parent','x','y','width','height'],['id','label','kind','x','y','width','height'],'group')
        gid = ident(group['id'])
        if gid in group_map or group['kind'] not in GROUPS:
            raise ValueError('duplicate group ID or unknown group kind')
        checked_text(group['label'], 'group label', 100)
        r = rect(group)
        if min(r[2:]) < 70 or not inside(r, drawing):
            raise ValueError(f'group {gid} is too small or outside drawing area')
        group_map[gid], rects[gid] = group, r
        headers.append((r[0]+1, r[1]+1, min(r[2]-2,text_width(group['label'],13)+28), 36))
    for gid, group in group_map.items():
        parents, current = [], group.get('parent')
        while current is not None:
            if current not in group_map or current in parents or current == gid:
                raise ValueError('unknown parent or cyclic group hierarchy')
            parents.append(current)
            current = group_map[current].get('parent')
        ancestors[gid] = parents
        if parents:
            x,y,w,h = rects[parents[0]]
            if not inside(rects[gid], (x+12,y+42,w-24,h-54)):
                raise ValueError(f'group {gid} extends outside parent content')
    for i, a in enumerate(groups):
        for b in groups[i+1:]:
            if a['id'] not in ancestors[b['id']] and b['id'] not in ancestors[a['id']] and overlaps(rects[a['id']],rects[b['id']]):
                raise ValueError('unrelated group boundaries overlap')
    used, prepared = {}, {}
    for node in nodes:
        fields(node,['id','icon','symbol','label','detail','badge','group','x','y','width','height'],
               ['id','label','x','y'],'node')
        nid = ident(node['id'])
        if nid in node_map or nid in group_map:
            raise ValueError('node/group IDs must be unique')
        r = rect({'width':208,'height':112,**node})
        if r[2] < 160 or r[3] < 100 or r[2] > 360 or r[3] > 190 or not inside(r,drawing):
            raise ValueError(f'node {nid}: invalid dimensions or outside drawing area')
        checked_text(node['label'], 'node label', 100)
        checked_text(node.get('detail',''), 'node detail', 100, True)
        checked_text(node.get('badge',''), 'node badge', 24, True)
        parent = node.get('group')
        if parent is not None:
            if parent not in group_map:
                raise ValueError('unknown node group')
            x,y,w,h = rects[parent]
            if not inside(r,(x+12,y+42,w-24,h-54)):
                raise ValueError(f'node {nid} extends outside group content')
        for gid in group_map:
            if overlaps(r,rects[gid]) and gid != parent and gid not in ancestors.get(parent,[]):
                raise ValueError(f'node {nid} overlaps unrelated group {gid}')
        if ('icon' in node) == ('symbol' in node):
            raise ValueError('node needs exactly one official icon or generic symbol')
        uri = None
        if 'icon' in node:
            item, uri = resolve(catalog,cache,node['icon'])
            if spec['provider'] != 'neutral' and item['provider'] != spec['provider']:
                raise ValueError('mixed provider icons require neutral provider')
            used[item['id']] = item
        elif node['symbol'] not in {'user','external','interface'}:
            raise ValueError('unknown generic symbol')
        prepared[nid], node_map[nid], rects[nid] = uri, node, r
    for i,a in enumerate(nodes):
        for b in nodes[i+1:]:
            if overlaps(expand(rects[a['id']],8),expand(rects[b['id']],8)):
                raise ValueError('nodes overlap or leave less than 16px clearance')
    obstacles = [expand(rects[n['id']],12) for n in nodes] + headers
    notes = spec.get('notes',[])
    if not isinstance(notes,list) or len(notes)>5:
        raise ValueError('at most five notes')
    sources = spec.get('sources',[])
    if not isinstance(sources,list) or len(sources)>5:
        raise ValueError('at most five sources')
    for source in sources:
        fields(source,['title','url'],['title','url'],'source')
        checked_text(source['title'],'source title',100)
        if not isinstance(source['url'],str) or len(source['url'])>2000 or urlparse(source['url']).scheme!='https' or not urlparse(source['url']).netloc:
            raise ValueError('source must be an HTTPS URL')
    warnings = []
    if not sources:
        warnings.append('No architecture references supplied; technical review required.')
    for item in used.values():
        if item['kind']=='category':
            warnings.append(f'{item["id"]}: official category icon; verify the product/category mapping.')
    routed, occupied, label_boxes, edge_ids = [], [], [], set()
    for index,edge in enumerate(edges):
        fields(edge,['id','from','to','kind','label','from_port','to_port','from_offset','to_offset','via'],['from','to','kind'],'edge')
        eid=ident(edge.get('id',f'e{index+1}'))
        if eid in edge_ids or edge['from'] not in node_map or edge['to'] not in node_map or edge['from']==edge['to'] or edge['kind'] not in FLOW:
            raise ValueError('duplicate edge, unknown endpoint/type, or self edge')
        edge_ids.add(eid)
        ra,rb=rects[edge['from']],rects[edge['to']]
        dx,dy=rb[0]-ra[0],rb[1]-ra[1]
        default_a,default_b=(('e','w') if dx>=0 else ('w','e')) if abs(dx)>=abs(dy) else (('s','n') if dy>=0 else ('n','s'))
        sa,sb=edge.get('from_port',default_a),edge.get('to_port',default_b)
        if sa not in PORTS or sb not in PORTS:
            raise ValueError('ports must be n, e, s or w')
        a=port(ra,sa,number(edge.get('from_offset',0),'port offset',-.35,.35))
        b=port(rb,sb,number(edge.get('to_offset',0),'port offset',-.35,.35))
        gap = (b[0]-a[0] if (sa,sb)==('e','w') else a[0]-b[0] if (sa,sb)==('w','e') else
               b[1]-a[1] if (sa,sb)==('s','n') else a[1]-b[1] if (sa,sb)==('n','s') else 0)
        # Facing ports share their available channel; overlapping jetties create tiny loops.
        preferred=min(jetty,gap/2) if gap>=24 else jetty
        def stub(p,side):
            for distance in sorted({preferred,*[d for d in (24,20,16,12) if d<preferred]},reverse=True):
                candidate=(p[0]+PORTS[side][0]*distance,p[1]+PORTS[side][1]*distance)
                if inside((*candidate,0,0),drawing) and not any(segment_hits(candidate,candidate,r) for r in obstacles):
                    return candidate
            raise ValueError(f'edge {eid}: no clearance at {side} port')
        astub,bstub=stub(a,sa),stub(b,sb)
        via=edge.get('via',[])
        if not isinstance(via,list) or len(via)>12:
            raise ValueError('at most twelve via points')
        waypoints=[astub]
        for p in via:
            if not isinstance(p,list) or len(p)!=2:
                raise ValueError('via point must be [x,y]')
            waypoints.append((number(p[0],'via x'),number(p[1],'via y')))
        waypoints.append(bstub)
        points=[a]
        for leg,(start,end) in enumerate(zip(waypoints,waypoints[1:])):
            try:
                heading={'e':0,'s':1,'w':2,'n':3}
                points.extend(route(start,end,obstacles,drawing,occupied,
                                    heading[sa] if leg==0 else -1,
                                    (heading[sb]+2)%4 if leg==len(waypoints)-2 else -1))
            except ValueError as error:
                raise ValueError(f'edge {eid} ({edge["from"]}->{edge["to"]}): {error}') from error
        points=simplify(points+[b])
        for start,end in zip(points,points[1:]):
            for node in nodes:
                if node['id'] not in (edge['from'],edge['to']) and segment_hits(start,end,expand(rects[node['id']],4)):
                    raise ValueError('port stub intersects another node')
            if any(segment_hits(start,end,r) for r in headers):
                raise ValueError('route crosses a boundary title')
        occupied.extend(zip(points,points[1:]))
        lengths=[abs(a[0]-b[0])+abs(a[1]-b[1]) for a,b in zip(points,points[1:])]
        direct=abs(points[0][0]-points[-1][0])+abs(points[0][1]-points[-1][1])
        reversal=any((b[0]-a[0])*(c[0]-b[0])+(b[1]-a[1])*(c[1]-b[1])<0 for a,b,c in zip(points,points[1:],points[2:]))
        if reversal:
            raise ValueError(f'edge {eid}: waypoint introduces a 180-degree reversal; adjust via points')
        routed.append(dict(id=eid,kind=edge['kind'],points=points,label=edge.get('label',''),source=edge['from'],target=edge['to'],
                           bends=max(0,len(points)-2),length=round(sum(lengths),2),detour_ratio=round(sum(lengths)/max(direct,1),3),
                           first_segment=lengths[0],last_segment=lengths[-1]))
    # Labels are placed after routing so no connection is hidden under another label.
    for edge in routed:
        value=checked_text(edge['label'],'edge label',48,True)
        if not value:
            continue
        tw=text_width(value,12)+18
        candidates=[]
        for a,b in zip(edge['points'],edge['points'][1:]):
            length=abs(a[0]-b[0])+abs(a[1]-b[1])
            if a[1]==b[1] and length>=tw+8:
                for fraction in (.5,.3,.7):
                    cx=a[0]+(b[0]-a[0])*fraction
                    candidates += [(cx-tw/2,a[1]-27,tw,22),(cx-tw/2,a[1]+5,tw,22)]
            elif a[0]==b[0] and length>=30:
                for fraction in (.5,.25,.75,.3,.7,.1,.9):
                    cy=a[1]+(b[1]-a[1])*fraction
                    candidates += [(a[0]+7,cy-11,tw,22),(a[0]-tw-7,cy-11,tw,22)]
        selected=next((r for r in candidates if inside(r,drawing) and
                       not any(overlaps(r,o) for o in obstacles+label_boxes) and
                       not any(segment_hits(a,b,r) for a,b in occupied)),None)
        if selected is None:
            raise ValueError(f'edge {edge["id"]}: no readable label position; shorten label or move nodes')
        edge['label_box']=selected
        label_boxes.append(expand(selected,3))
    crossings=[]
    for i,a in enumerate(routed):
        for b in routed[i+1:]:
            if any(segments_intersect(p,q,r,s) for p,q in zip(a['points'],a['points'][1:]) for r,s in zip(b['points'],b['points'][1:])):
                crossings.append([a['id'],b['id']])
    if crossings:
        warnings.append(f'{len(crossings)} edge pairs intersect or share a corridor; crossings without dots are not junctions. Review routes.')
    report=dict(schema_version=2,title=title,provider=spec['provider'],node_count=len(nodes),edge_count=len(edges),
                group_count=len(groups),warnings=warnings,edge_intersections=crossings,routes=routed,
                sources=sources,icons=[used[k] for k in sorted(used)],canvas=canvas,
                quality={'node_overlaps':0,'route_node_collisions':0,'label_collisions':0,'boundary_title_collisions':0,
                         'total_bends':sum(e['bends'] for e in routed),'maximum_bends':max((e['bends'] for e in routed),default=0),
                         'short_terminal_segments':sum(e[k]<24 for e in routed for k in ('first_segment','last_segment'))},
                edge_style={'corner':corner,'radius':radius,'width':line_width,'jetty':jetty},
                limitations=['Geometry checks do not validate cloud configuration, availability, IAM, or security.'])
    svg=[f'<svg xmlns="http://www.w3.org/2000/svg" width="{width:g}px" height="{height:g}px" viewBox="0 0 {width:g} {height:g}" font-family="Arial, Hiragino Sans, Noto Sans CJK JP, sans-serif">',
         f'<title>{esc(title)}</title><desc>{esc(subtitle)}</desc>',f'<metadata>{esc(json.dumps(report,ensure_ascii=False))}</metadata>',
         f'<rect width="{width:g}" height="{height:g}" fill="white"/>',f'<rect width="8" height="{height:g}" fill="{accent}"/>',
         svg_text(provider+'  /  SOLUTION ARCHITECTURE',40,35,12,accent,700,1000),
         svg_text(title,40,82,32,'#182b43',700,width-100,1,'title'),
         svg_text(subtitle,40,116,14,'#607087',400,width-100,1,'subtitle'),
         f'<path d="M40 140 H{width-40:g}" stroke="#dce3eb"/>','<defs>']
    for kind,(color,_,_) in FLOW.items():
        svg.append(f'<marker id="arrow-{kind}" viewBox="0 0 10 10" refX="10" refY="5" markerUnits="userSpaceOnUse" markerWidth="9" markerHeight="9" orient="auto"><path d="M0 0 L10 5 L0 10z" fill="{color}"/></marker>')
    svg.append('</defs>')
    for group in sorted(groups,key=lambda g:(len(ancestors[g['id']]),g['id'])):
        x,y,w,h=rects[group['id']]
        kind=group['kind']
        fill={'cloud':'#fcfdff','region':'#fafbfd','vpc':'#f2f7fc','zone':'#fcfcff','subnet':'#ffffff','service':'#f5f9f7','logical':'#f9fafc'}[kind]
        stroke=accent if kind in ('cloud','vpc') else '#b9c7d5'
        dash='7 5' if kind in ('zone','region','logical') else ''
        svg += [f'<g id="group-{group["id"]}" data-kind="{kind}"><rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}" rx="8" fill="{fill}" stroke="{stroke}" stroke-width="1.3" stroke-dasharray="{dash}"/>',
                svg_text(group['label'],x+14,y+25,13,accent if kind in ('cloud','vpc') else '#566b83',700,w-28,1,'group '+group['id']),'</g>']
    for edge in routed:
        color,dash,_=FLOW[edge['kind']]
        d=connector_path(edge['points'],radius if corner=='rounded' else 0,
                         [expand(rects[n['id']],4) for n in nodes]+headers+label_boxes)
        svg += [f'<g id="edge-{edge["id"]}" data-kind="{edge["kind"]}"><title>{esc(edge["label"] or edge["source"]+" → "+edge["target"])}</title>',
                f'<path d="{d}" fill="none" stroke="white" stroke-width="{line_width+4:g}" stroke-linejoin="miter"/>',
                f'<path d="{d}" fill="none" stroke="{color}" stroke-width="{line_width:g}" stroke-dasharray="{dash}" stroke-linejoin="miter" marker-end="url(#arrow-{edge["kind"]})"/></g>']
    for node in nodes:
        nid=node['id'];x,y,w,h=rects[nid]
        svg += [f'<g id="node-{nid}"><title>{esc(node["label"])}</title>',
                f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}" rx="8" fill="white" stroke="#b9c8d8" stroke-width="1.2"/>']
        if prepared[nid]:
            svg.append(f'<image href="{prepared[nid]}" x="{x+14:g}" y="{y+28:g}" width="48" height="48" preserveAspectRatio="xMidYMid meet"/>')
        else:
            symbol=node['symbol']
            d='M24 29a9 9 0 1 0 0-18a9 9 0 1 0 0 18 M8 47v-5c0-13 32-13 32 0v5' if symbol=='user' else 'M6 12h36v26H6z M17 45h14 M24 38v7' if symbol=='external' else 'M4 24h40 M13 15l-9 9 9 9 M35 15l9 9-9 9'
            svg.append(f'<g transform="translate({x+14:g} {y+16:g})" fill="none" stroke="#596e84" stroke-width="2.2"><path d="{d}"/></g>')
        svg.append(svg_text(node['label'],x+76,y+34,15,'#20344b',600,w-88,3,'node '+nid))
        if node.get('detail'):
            svg.append(svg_text(node['detail'],x+14,y+h-15,11,'#62748a',400,w-28,1,'detail '+nid))
        if node.get('badge'):
            bw=text_width(node['badge'],9)+16
            if bw>w-20:
                raise ValueError('badge too wide')
            svg += [f'<rect x="{x+9:g}" y="{y+4:g}" width="{bw:g}" height="16" rx="8" fill="{accent}"/>',svg_text(node['badge'],x+17,y+15,9,'white',700,bw-12)]
        svg.append('</g>')
    for edge in routed:
        if 'label_box' in edge:
            x,y,w,h=edge['label_box']
            svg += [f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}" rx="4" fill="white"/>',
                    svg_text(edge['label'],x+9,y+15,12,FLOW[edge['kind']][0],500,w-16)]
    # A separate editorial rail keeps operational context out of connection routes.
    nx=width-325; ny=160
    svg += [f'<path d="M{nx-25:g} 160 V{height-150:g}" stroke="#dce3eb"/>',svg_text('設計の要点',nx,ny+22,18,'#20344b',700,280)]
    ny+=54
    for i,note in enumerate(notes):
        fields(note,['title','body'],['title','body'],'note')
        heading=checked_text(note['title'],'note title',60)
        body=checked_text(note['body'],'note body',300)
        body_lines=lines(body,260,13,7,'note body')
        svg += [svg_text(f'{i+1:02d}',nx,ny+15,12,accent,700,40),svg_text(heading,nx+32,ny+16,14,'#20344b',700,230,2,'note heading'),
                svg_text(body,nx,ny+49,13,'#617187',400,260,7,'note body')]
        ny+=65+len(body_lines)*18.85
    if ny>height-195:
        raise ValueError('notes exceed sidebar height')
    footer=height-120
    svg += [f'<path d="M40 {footer-15:g} H{width-40:g}" stroke="#dce3eb"/>']
    lx=40
    for kind in FLOW:
        if not any(e['kind']==kind for e in routed):
            continue
        color,dash,label=FLOW[kind]
        svg += [f'<path d="M{lx:g} {footer+10:g} h32" stroke="{color}" stroke-width="2.2" stroke-dasharray="{dash}"/>',svg_text(label,lx+42,footer+15,12,'#52667d',400,150)]
        lx+=200
    svg.append(svg_text('交差に接続点なし = 非接続',width-325,footer+15,11,'#62748a',400,280))
    for i,source in enumerate(sources):
        # Keep output self-contained for docsvg reverse; URLs remain in metadata/report.
        svg.append(svg_text(f'[{i+1}] '+source['title'],40+(i%2)*(width-430)/2,footer+45+(i//2)*19,10,'#617187',400,(width-460)/2,1,'source title'))
    svg.append(svg_text(revision,width-325,height-36,11,accent,600,280))
    svg.append('</svg>\n')
    return '\n'.join(svg),report
