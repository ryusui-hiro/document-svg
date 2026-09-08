"""Reusable reference templates and local, portable review/export bundles."""
import html
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

TEMPLATES = Path(__file__).with_name('templates')
NAMES = ('azure-private-web', 'aws-multi-az-web', 'gcp-serverless-web')


def template(name):
    if name not in NAMES:
        raise ValueError('unknown template; choose ' + ', '.join(NAMES))
    return json.loads((TEMPLATES / (name + '.json')).read_text(encoding='utf-8'))


def list_templates():
    return [dict(name=name, provider=template(name)['provider'], title=template(name)['title']) for name in NAMES]


def build(output, catalog, cache, render, png_width=2400):
    output=Path(output)
    if output.exists():
        raise ValueError(f'output already exists: {output}')
    if type(png_width) is not int or not 1200 <= png_width <= 4800:
        raise ValueError('PNG width must be 1200..4800')
    binary=shutil.which('rsvg-convert')
    if not binary:
        raise ValueError('rsvg-convert is required for PNG previews; SVG-only render remains available')
    output.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.architecture-',dir=output.parent) as temporary:
        stage=Path(temporary)
        items=[]
        for name in NAMES:
            spec=template(name)
            for corner,suffix in [('sharp',''),('rounded','.rounded')]:
                variant={**spec,'edge_style':{'corner':corner,'radius':8,'width':1.65,'jetty':32}}
                svg,report=render(variant,catalog,cache)
                stem=name+suffix
                (stage/(stem+'.svg')).write_text(svg,encoding='utf-8')
                (stage/(stem+'.json')).write_text(json.dumps(variant,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
                (stage/(stem+'.report.json')).write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
                subprocess.run([binary,'-w',str(png_width),str(stage/(stem+'.svg')),'-o',str(stage/(stem+'.png'))],check=True,capture_output=True,timeout=60)
            items.append(dict(name=name,title=spec['title'],provider=spec['provider'],sources=spec.get('sources',[]),warnings=report['warnings'],quality=report['quality']))
        review(stage,items)
        manifest=dict(schema_version=1,templates=items,png_width=png_width,icon_snapshot=catalog.get('verified_on'))
        (stage/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        # Every artifact has been rendered before creating the user-visible bundle.
        output.mkdir()
        for path in stage.iterdir():
            shutil.move(str(path),str(output/path.name))
    return dict(output=str(output.resolve()),index=str((output/'index.html').resolve()),**manifest)


def review(output,items):
    panels=[];buttons=[]
    for i,item in enumerate(items):
        name=item['name'];title=html.escape(item['title']);short={'azure':'Azure','aws':'AWS','gcp':'Google Cloud'}[item['provider']]
        buttons.append(f'<button id="tab-{name}" type="button" role="tab" aria-selected="{str(i==0).lower()}" aria-controls="panel-{name}" tabindex="{0 if i==0 else -1}" data-panel="panel-{name}">{short}</button>')
        links=''.join(f'<a href="{name}.{ext}" data-file="{name}" data-extension="{ext}" download>{label}</a>' for ext,label in [('svg','SVG'),('png','高解像度PNG'),('json','構成JSON'),('report.json','検証結果')])
        sources=''.join(f'<li><a href="{html.escape(s["url"],quote=True)}">{html.escape(s["title"])}</a></li>' for s in item['sources'])
        warnings=''.join(f'<li>{html.escape(w)}</li>' for w in item['warnings'])
        panels.append(f'<section id="panel-{name}" role="tabpanel" aria-labelledby="tab-{name}" {"hidden" if i else ""}><div class="panel-heading"><h2>{title}</h2><div class="downloads">{links}</div></div><div class="drawing"><img src="{name}.svg" data-diagram="{name}" alt="{title}"/></div><div class="evidence"><div><h3>参照した公式資料</h3><ul>{sources}</ul></div><div><h3>確認事項</h3><p>重なり・ノードを貫く配線・ラベル衝突：検出なし</p>{"<ul>"+warnings+"</ul>" if warnings else "<p>配線の交差・共有区間：検出なし</p>"}<p>構成の説明用テンプレートです。実際のSKU、IAM、経路、容量、可用性は要件に合わせてレビューしてください。</p></div></div></section>')
    page='''<!doctype html><html lang="ja"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; script-src 'unsafe-inline'"><title>Cloud Architecture Studio</title><style>
*{box-sizing:border-box}body{margin:0;background:#f1f4f8;color:#182b43;font:14px -apple-system,BlinkMacSystemFont,'Hiragino Sans',sans-serif}header{padding:24px 32px;background:#12273e;color:white}header p{margin:6px 0 0;color:#c6d6e6}h1{margin:0;font-size:24px}main{padding:24px 32px}nav{display:flex;gap:8px;flex-wrap:wrap;margin-bottom:18px}button{padding:12px 24px;border:1px solid #c5d0dd;border-radius:8px;background:white;color:#243d58;font:inherit;cursor:pointer}button[aria-selected=true]{background:#1f5f9f;color:white;border-color:#1f5f9f}button:focus-visible,a:focus-visible{outline:3px solid #e2a52b;outline-offset:3px}section{background:white;border:1px solid #d3dde8;border-radius:12px;overflow:hidden}section[hidden]{display:none}.panel-heading{padding:20px 24px;display:flex;align-items:center;justify-content:space-between;gap:16px;flex-wrap:wrap}h2{font-size:18px;margin:0}.downloads{display:flex;gap:14px;flex-wrap:wrap}a{color:#195c9d;text-underline-offset:3px}.drawing{border-top:1px solid #e5eaf0;border-bottom:1px solid #e5eaf0}img{display:block;width:100%;height:auto}.evidence{padding:16px 24px;display:grid;grid-template-columns:1fr 1fr;gap:30px;font-size:13px;line-height:1.8;color:#566b82}h3{color:#243d58;font-size:14px}ul{padding-left:20px;overflow-wrap:anywhere}@media(max-width:650px){header{padding:20px 16px}main{padding:16px 10px}.panel-heading{padding:16px}.evidence{grid-template-columns:1fr;padding:16px;gap:0}h2{line-height:1.6}button{padding:10px 18px}}
</style></head><body><header><h1>Cloud Architecture Studio</h1><p>公式アイコンと参照アーキテクチャから作る、業務資料向けの構成図</p></header><main><nav role="tablist" aria-label="クラウドを選択">'''+''.join(buttons)+'''</nav>'''+''.join(panels)+'''</main><script>
const tabs=[...document.querySelectorAll('[role=tab]')];function activate(tab){for(const b of tabs){const active=b===tab;b.setAttribute('aria-selected',String(active));b.tabIndex=active?0:-1;document.getElementById(b.dataset.panel).hidden=!active;}}for(const [i,tab] of tabs.entries()){tab.addEventListener('click',()=>activate(tab));tab.addEventListener('keydown',event=>{let target;if(event.key==='ArrowRight')target=tabs[(i+1)%tabs.length];if(event.key==='ArrowLeft')target=tabs[(i+tabs.length-1)%tabs.length];if(event.key==='Home')target=tabs[0];if(event.key==='End')target=tabs[tabs.length-1];if(target){event.preventDefault();activate(target);target.focus();}});}
</script></body></html>'''
    page=page.replace('</nav>', '</nav><div style="margin-bottom:18px"><label for="corner">コネクタの角：</label><select id="corner" style="padding:8px;font:inherit"><option value="sharp">直角（標準）</option><option value="rounded">小さな角丸</option></select></div>',1)
    page=page.replace('</script>', '''document.getElementById('corner').addEventListener('change',event=>{const suffix=event.target.value==='rounded'?'.rounded':'';for(const img of document.querySelectorAll('img[data-diagram]'))img.src=img.dataset.diagram+suffix+'.svg';for(const a of document.querySelectorAll('a[data-file]'))a.href=a.dataset.file+suffix+'.'+a.dataset.extension;});</script>''',1)
    (output/'index.html').write_text(page,encoding='utf-8')
