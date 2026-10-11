"""Development-only: freeze the existing UI's geometry and artwork for Win32.

Requires Playwright and a Chromium browser. No browser is used to lay out the
native application at runtime. Reference screenshots live outside the checkout.
"""
import argparse
import base64
import json
import re
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'src-tauri' / 'assets'
EXPORT = r"""() => {
  const nodes = [];
  const visible = e => {const r=e.getBoundingClientRect();const s=getComputedStyle(e);return r.width && r.height && s.visibility!=='hidden' && s.display!=='none';};
  const box = r => [r.x,r.y,r.width,r.height];
  for(const e of document.querySelectorAll('body *')) {
    if(!visible(e) || e.closest('svg') || ['SCRIPT','STYLE','OPTION'].includes(e.tagName)) continue;
    const s=getComputedStyle(e), r=e.getBoundingClientRect();
    const owner=e.closest('button,input,select,[id]');
    const id=e.id || '';
    let opacity=1;for(let p=e;p;p=p.parentElement)opacity*=parseFloat(getComputedStyle(p).opacity);
    const wrap=r.width-parseFloat(s.paddingLeft)-parseFloat(s.paddingRight)-parseFloat(s.borderLeftWidth)-parseFloat(s.borderRightWidth);
    const common={id,owner:owner?.id || (owner?.dataset.tab ? 'tab:'+owner.dataset.tab : ''),box:box(r),classes:e.className || '',color:s.color,size:parseFloat(s.fontSize),weight:parseInt(s.fontWeight),line:parseFloat(s.lineHeight)||parseFloat(s.fontSize)*1.2,spacing:parseFloat(s.letterSpacing)||0,align:s.textAlign,opacity,wrap,nowrap:s.whiteSpace==='nowrap',title:e.title||'',disabled:!!e.disabled};
    if(e.matches('input:not([type=checkbox]),select')) {
      nodes.push({...common,kind:e.tagName==='SELECT'?'select':'edit',text:e.tagName==='SELECT'?e.selectedOptions[0]?.textContent:e.value,placeholder:e.placeholder||'',password:e.type==='password',background:s.backgroundColor,border:s.borderTopColor,width:parseFloat(s.borderTopWidth),radius:parseFloat(s.borderTopLeftRadius),padding:[parseFloat(s.paddingLeft),parseFloat(s.paddingTop),parseFloat(s.paddingRight),parseFloat(s.paddingBottom)],options:e.tagName==='SELECT'?[...e.options].map(o=>[o.value,o.textContent]):[]});
      continue;
    }
    if(e.matches('input[type=checkbox]')) continue;
    if(s.backgroundColor!=='rgba(0, 0, 0, 0)' || parseFloat(s.borderTopWidth)>0) nodes.push({...common,kind:'rect',background:s.backgroundColor,border:s.borderTopColor,width:parseFloat(s.borderTopWidth),radius:parseFloat(s.borderTopLeftRadius),dashed:s.borderTopStyle==='dashed'});
    if(e.tagName==='IMG') nodes.push({...common,kind:'image',src:e.getAttribute('src')});
    for(const child of e.childNodes) {
      if(child.nodeType!==Node.TEXT_NODE || !child.textContent.trim())continue;
      const range=document.createRange();range.selectNodeContents(child);
      let text=child.textContent.trim();if(s.textTransform==='uppercase')text=text.toUpperCase();
      nodes.push({...common,kind:'text',box:box(range.getBoundingClientRect()),text});
    }
    if(e.matches('button,.sub-file-zone,.toggle-switch')) nodes.push({...common,kind:'hit',owner:e.id || (e.dataset.tab?'tab:'+e.dataset.tab:e.querySelector('input')?.id||'')});
  }
  for(const e of document.querySelectorAll('svg')) {
    if(!visible(e))continue;
    const s=getComputedStyle(e),r=e.getBoundingClientRect();
    const svg=e.cloneNode(true);svg.setAttribute('xmlns','http://www.w3.org/2000/svg');svg.setAttribute('color',s.color);
    const owner=e.closest('button');
    let opacity=1;for(let p=e;p;p=p.parentElement)opacity*=parseFloat(getComputedStyle(p).opacity);
    nodes.push({kind:'icon',id:e.parentElement.id,owner:owner?.id || (owner?.dataset.tab?'tab:'+owner.dataset.tab:''),box:box(r),svg:svg.outerHTML,color:s.color,opacity});
  }
  return {nodes,height:document.querySelector('.tab-panel.active').scrollHeight};
}"""

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--browser', default=r'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    ASSETS.mkdir(parents=True, exist_ok=True)
    scenes, icons = {}, {}
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=args.browser, headless=True)
        page = browser.new_page(viewport={'width':520,'height':580}, device_scale_factor=1)
        html=(ROOT/'src'/'index.html').read_text(encoding='utf8')
        html=re.sub(r'<script\b[^>]*>[\s\S]*?</script>', '', html)
        html=re.sub(r'<link\b[^>]*>', '', html)
        for logo in ('annotate_logowithtext_solid.svg','annotate_logowithtext_white.svg'):
            data=base64.b64encode((ROOT/'src'/logo).read_bytes()).decode()
            html=html.replace('./'+logo,'data:image/svg+xml;base64,'+data)
        page.set_content(html)
        page.add_style_tag(content=(ROOT/'src'/'index.css').read_text(encoding='utf8'))
        page.add_style_tag(content='*,*::before,*::after {animation:none!important;transition:none!important;}'+''.join(f"@font-face{{font-family:Satoshi;font-weight:{w};src:url(data:font/otf;base64,{base64.b64encode((ASSETS/'fonts'/f'Satoshi-{w}.otf').read_bytes()).decode()});}}" for w in (300,400,500,700)))
        # Offline profile-photo fixtures exercise PNG/JPEG decoding and circular cropping.
        avatar=page.evaluate("""() => {const c=document.createElement('canvas');c.width=96;c.height=64;const x=c.getContext('2d');for(const [color,a,b]of [['#517b9b',0,0],['#ae7864',48,0],['#80977b',0,32],['#c9ac73',48,32]]){x.fillStyle=color;x.fillRect(a,b,48,32);}return [c.toDataURL('image/png'),c.toDataURL('image/jpeg',.9)];}""")
        for data,extension in zip(avatar,('png','jpg')):
            (ASSETS/f'profile-avatar-fixture.{extension}').write_bytes(base64.b64decode(data.split(',')[1]))
        page.evaluate("s=>document.getElementById('sync-user-avatar').src=s",avatar[0])
        page.evaluate('document.fonts.ready')
        page.evaluate('''() => {
          const el=id=>document.getElementById(id), show=(id,value)=>el(id).style.display=value?'':'none';
          el('history-list').innerHTML='<li class="history-empty">No transcriptions yet</li>';
          el('dict-list').innerHTML='<li class="dict-empty">No terms added yet</li>';
          el('mic-select').innerHTML='<option value="">System Default</option>';
          el('whisper-status-text').textContent='Model not downloaded (~600 MB)';show('whisper-download-btn',true);
          el('gec-status-text').textContent='Model not downloaded (~105 MB)';show('gec-download-btn',true);
          for(const key of ['ffmpeg','vad']){el('sub-dep-'+key+'-status').textContent='Not installed';show('sub-'+key+'-download-btn',true);}
          for(const tab of document.querySelectorAll('[data-tab]'))tab.addEventListener('click',()=>{for(const b of document.querySelectorAll('[data-tab]'))b.classList.toggle('active',b===tab);for(const p of document.querySelectorAll('.tab-panel'))p.classList.toggle('active',p.id==='tab-'+tab.dataset.tab);});
          const mode=()=>{const value=el('mode-select').value;show('section-apikey',value!=='local');show('section-azure-config',value==='azure-mai');show('section-local-whisper',value==='local');el('apikey-label').textContent=value==='azure-mai'?'Foundry API Key':'API Key';el('apikey-input').placeholder=value==='azure-mai'?'Paste Foundry API key':'Paste Groq API key';};
          el('mode-select').addEventListener('change',mode);mode();
          el('titlebar-close').addEventListener('click',()=>show('close-modal',true));el('modal-cancel').addEventListener('click',()=>show('close-modal',false));
        }''')
        def capture(key, screenshot=False):
            scene=page.evaluate(EXPORT)
            for node in scene['nodes']:
                if node['kind']=='icon':
                    svg=node.pop('svg')
                    if svg not in icons:
                        index=len(icons);icons[svg]=index
                        image=page.evaluate("""async ([svg,w,h])=>{const img=new Image();img.src='data:image/svg+xml;base64,'+btoa(svg);await img.decode();const c=document.createElement('canvas');c.width=w*3;c.height=h*3;c.getContext('2d').drawImage(img,0,0,c.width,c.height);return c.toDataURL('image/png').split(',')[1];}""", [svg,node['box'][2],node['box'][3]])
                        (ASSETS/f'icon-{index}.png').write_bytes(base64.b64decode(image))
                    node['src']=f'icon-{icons[svg]}.png'
                elif node['kind']=='image' and node.get('src','').startswith('data:'):
                    node['src']='profile-avatar-fixture.png' if node['id']=='sync-user-avatar' else ('annotate_logowithtext_white.png' if 'logo-dark' in node.get('classes','') else 'annotate_logowithtext_solid.png')
            scenes[key]=scene
            if screenshot:page.screenshot(path=str(args.output/f'{key.replace(":","-")}.png'))
        for theme in ('light','dark'):
            page.evaluate('(t)=>{document.documentElement.setAttribute("data-theme",t);document.getElementById("theme-toggle").checked=t==="dark";}',theme)
            for tab in ('home','dictionary','subtitling'):
                page.locator(f'[data-tab={tab}]').click()
                capture(f'{theme}:{tab}',True)
            page.locator('[data-tab=settings]').click()
            for mode in ('groq','azure-mai','local'):
                page.locator('#mode-select').select_option(mode)
                for accel in ('vulkan',):
                    if mode=='local':page.locator('#acceleration-select').select_option(accel)
                    for grammar in (False,True):
                        page.evaluate('(v)=>{document.querySelector("#grammar-toggle").checked=v;document.querySelector("#gec-model-section").style.display=v?"":"none"}',grammar)
                        for sync in ('out','waiting','in'):
                            page.evaluate('''s=>{for(const [id,key]of [['sync-signed-out','out'],['sync-waiting','waiting'],['sync-signed-in','in']])document.getElementById(id).style.display=s===key?'':'none';document.getElementById('sync-user-name').textContent='Example User';document.getElementById('sync-user-email').textContent='exa***@example.com';document.getElementById('tab-settings').scrollTop=0;}''',sync)
                            capture(f'{theme}:settings:{mode}:{accel}:{int(grammar)}:{sync}',sync=='out' and not grammar)
            page.evaluate("document.getElementById('tab-settings').scrollTop=10000")
            page.screenshot(path=str(args.output/f'{theme}-settings-bottom.png'))
            page.locator('[data-tab=home]').click()
            page.locator('#titlebar-close').click()
            capture(f'{theme}:modal',True)
            page.locator('#modal-cancel').click()
            entries=[{'index':i+1,'start':i*3.0,'end':i*3.0+2.5,'text':text} for i,text in enumerate(['Welcome to Annotate.','This is a native Windows transcription app.','Vulkan runs the local model on your GPU.'])]
            srt='\n\n'.join(f"{e['index']}\n00:00:{int(e['start']):02d},000 --> 00:00:{int(e['end']):02d},500\n{e['text']}" for e in entries)
            page.locator('[data-tab=subtitling]').click()
            page.evaluate('''([entries,srt])=>{for(const id of ['sub-desc','sub-file-zone','sub-lang-group','sub-deps','sub-generate-btn','sub-progress'])document.getElementById(id).style.display='none';document.getElementById('sub-result').style.display='';document.getElementById('sub-result-title').textContent='example';document.getElementById('sub-preview-count').textContent=entries.length+' entries';document.getElementById('sub-preview').textContent=srt;document.getElementById('sub-summarize-btn').disabled=false;}''',[entries,srt])
            for summary in ('','Annotate transcribes speech using a native Windows interface. It supports cloud services and a local Vulkan model.'):
                page.evaluate("s=>{document.getElementById('sub-summary').style.display=s?'':'none';document.getElementById('sub-summary-text').textContent=s;}",summary)
                key=f'{theme}:subtitling-'+('summary' if summary else 'result');capture(key,True)
                scenes[key]['fixture']={'file':'example.wav','entries':entries,'summary':summary,'openrouter':True}
            page.evaluate("() => {for(const id of ['sub-desc','sub-file-zone','sub-lang-group','sub-deps','sub-generate-btn'])document.getElementById(id).style.display='';for(const id of ['sub-result','sub-summary'])document.getElementById(id).style.display='none';}")
            for ready in (False,True):
                page.evaluate("v=>{document.getElementById('sub-file-zone').classList.add('has-file');document.getElementById('sub-file-name').textContent='example.wav';document.getElementById('sub-deps').style.display=v?'none':'';document.getElementById('sub-generate-btn').disabled=!v;}",ready)
                key=f'{theme}:subtitling-'+('ready' if ready else 'file');capture(key,True);scenes[key]['fixture']={'file':'example.wav','ready':['ffmpeg','vad'] if ready else []}
            page.evaluate("() => {document.getElementById('sub-file-zone').classList.remove('has-file');document.getElementById('sub-file-name').textContent='';document.getElementById('sub-deps').style.display='';document.getElementById('sub-generate-btn').disabled=true;}")
            for tab,values in [('dictionary',['Annotate','résumé','Vulkan']),('home',['A short transcription.','This longer transcription demonstrates wrapping across several lines while retaining the original card padding and spacing.','Native recording stays available when the settings window is closed.'])]:
                page.locator(f'[data-tab={tab}]').click()
                now=1700000000000
                page.evaluate('''([tab,values,time])=>{const dict=tab==='dictionary',list=document.getElementById(dict?'dict-list':'history-list');list.innerHTML='';document.getElementById(dict?'dict-count':'history-count').textContent=values.length;if(!dict)document.getElementById('history-clear').style.display='';for(const text of values){const li=document.createElement('li');li.className=dict?'dict-item':'history-item';if(!dict){const t=document.createElement('span');t.className='history-item-time';t.textContent=new Date(time).toLocaleDateString([],{month:'short',day:'numeric'})+' '+new Date(time).toLocaleTimeString([],{hour:'2-digit',minute:'2-digit'});li.append(t);}const s=document.createElement('span');s.className=dict?'dict-item-text':'history-item-text';s.textContent=text;li.append(s);if(dict){const b=document.createElement('button');b.className='dict-item-remove';b.textContent='Remove';li.append(b);}else{const a=document.createElement('span');a.className='history-item-actions';for(const action of ['Copy','Delete']){const b=document.createElement('button');b.className='history-item-'+action.toLowerCase().replace('delete','remove');b.textContent=action;a.append(b);}li.append(a);}list.append(li);}}''',[tab,values,now])
                key=f'{theme}:{tab}-filled';capture(key,True);scenes[key]['fixture']={tab:values,'time':now}
                page.evaluate("tab=>{const dict=tab==='dictionary';document.getElementById(dict?'dict-list':'history-list').innerHTML='<li class=\"'+(dict?'dict-empty':'history-empty')+'\">'+(dict?'No terms added yet':'No transcriptions yet')+'</li>';document.getElementById(dict?'dict-count':'history-count').textContent='';if(!dict)document.getElementById('history-clear').style.display='none';}",tab)
            for logo in ('annotate_logowithtext_solid.svg','annotate_logowithtext_white.svg'):
                src=(ROOT/'src'/logo).read_text(encoding='utf8')
                image=page.evaluate("""async svg=>{const img=new Image();img.src='data:image/svg+xml;base64,'+btoa(svg);await img.decode();const c=document.createElement('canvas');c.height=75;c.width=Math.round(img.width/img.height*75);c.getContext('2d').drawImage(img,0,0,c.width,c.height);return c.toDataURL('image/png').split(',')[1];}""",src)
                (ASSETS/Path(logo).with_suffix('.png').name).write_bytes(base64.b64decode(image))
        browser.close()
    (ASSETS/'native-reference.json').write_text(json.dumps(scenes,separators=(',',':')),encoding='utf8')
    print(f'Captured {len(scenes)} native reference scenes and {len(icons)} local icons')

if __name__=='__main__':main()
