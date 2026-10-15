"""Development-only: render the HTML/CSS design reference in Chromium.

Writes one PNG per screen state, named like `annotate.exe --capture-ui` output,
so the GPUI app can be compared against the reference. Requires Playwright and
a Chromium browser. Reference screenshots live outside the checkout.
"""
import argparse
import base64
import math
import re
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'src-tauri' / 'assets'
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--browser', default=r'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe')
    parser.add_argument('--scale', type=float, default=1, help='Pixel scale; the logical viewport stays 520 x 580')
    parser.add_argument('--platform', choices=('windows', 'macos'), default='windows', help='Platform-specific copy and titlebar reference')
    args = parser.parse_args()
    if not math.isfinite(args.scale) or args.scale <= 0:
        parser.error('--scale must be finite and greater than zero')
    args.output.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=args.browser, headless=True)
        page = browser.new_page(viewport={'width':520,'height':580}, device_scale_factor=args.scale)
        html=(ROOT/'src'/'index.html').read_text(encoding='utf8')
        html=re.sub(r'<script\b[^>]*>[\s\S]*?</script>', '', html)
        html=re.sub(r'<link\b[^>]*>', '', html)
        for logo in ('annotate_logowithtext_solid.svg','annotate_logowithtext_white.svg'):
            data=base64.b64encode((ROOT/'src'/logo).read_bytes()).decode()
            html=html.replace('./'+logo,'data:image/svg+xml;base64,'+data)
        page.set_content(html)
        page.add_style_tag(content=(ROOT/'src'/'index.css').read_text(encoding='utf8'))
        if args.platform == 'macos':
            page.evaluate('''() => {
              document.documentElement.dataset.platform='macos';
              for(const el of document.querySelectorAll('[data-macos]')) {
                el.textContent=el.dataset.macos;
                if(el.dataset.macosValue)el.value=el.dataset.macosValue;
              }
            }''')
        page.add_style_tag(content='*,*::before,*::after {animation:none!important;transition:none!important;}'+''.join(f"@font-face{{font-family:Satoshi;font-weight:{w};src:url(data:font/otf;base64,{base64.b64encode((ASSETS/'fonts'/f'Satoshi-{w}.otf').read_bytes()).decode()});}}" for w in (300,400,500,700)))
        # Offline profile-photo fixtures exercise PNG/JPEG decoding and circular cropping.
        avatar=page.evaluate("""() => {const c=document.createElement('canvas');c.width=96;c.height=64;const x=c.getContext('2d');for(const [color,a,b]of [['#517b9b',0,0],['#ae7864',48,0],['#80977b',0,32],['#c9ac73',48,32]]){x.fillStyle=color;x.fillRect(a,b,48,32);}return [c.toDataURL('image/png'),c.toDataURL('image/jpeg',.9)];}""")
        page.evaluate("s=>document.getElementById('sync-user-avatar').src=s",avatar[0])
        page.evaluate('document.fonts.ready')
        page.evaluate('''() => {
          const el=id=>document.getElementById(id), show=(id,value)=>el(id).style.display=value?'':'none';
          el('history-list').innerHTML='<li class="history-empty">No transcriptions yet</li>';
          el('dict-list').innerHTML='<li class="dict-empty">No terms added yet</li>';
          el('mic-select').innerHTML='<option value="">System Default</option>';
          show('setup-resume-btn',false);
          el('whisper-status-text').textContent='Model not downloaded (~600 MB)';show('whisper-download-btn',true);
          el('gec-status-text').textContent='Model not downloaded (~105 MB)';show('gec-download-btn',true);
          for(const key of ['ffmpeg','vad']){el('sub-dep-'+key+'-status').textContent='Not installed';show('sub-'+key+'-download-btn',true);}
          for(const tab of document.querySelectorAll('[data-tab]'))tab.addEventListener('click',()=>{for(const b of document.querySelectorAll('[data-tab]'))b.classList.toggle('active',b===tab);for(const p of document.querySelectorAll('.tab-panel'))p.classList.toggle('active',p.id==='tab-'+tab.dataset.tab);});
          const mode=()=>{const value=el('mode-select').value;show('section-apikey',value!=='local');show('section-azure-config',value==='azure-mai');show('section-local-whisper',value==='local');el('apikey-label').textContent=value==='azure-mai'?'Foundry API Key':'Groq API Key';el('apikey-input').placeholder=value==='azure-mai'?'Paste Foundry API key':'Paste Groq API key';};
          el('mode-select').addEventListener('change',mode);mode();
          el('titlebar-close').addEventListener('click',()=>show('close-modal',true));el('modal-cancel').addEventListener('click',()=>show('close-modal',false));
        }''')
        def capture(key, screenshot=True):
            if screenshot:page.screenshot(path=str(args.output/f'{key.replace(":","-")}.png'))
        for theme in ('light','dark'):
            page.evaluate('(t)=>{document.documentElement.setAttribute("data-theme",t);document.getElementById("theme-toggle").checked=t==="dark";}',theme)
            for tab in ('home','dictionary','subtitling'):
                page.locator(f'[data-tab={tab}]').click()
                capture(f'{theme}:{tab}')
            page.locator('[data-tab=settings]').click()
            for mode in ('groq','azure-mai','local'):
                page.locator('#mode-select').select_option(mode)
                for accel in (('metal',) if args.platform == 'macos' else ('vulkan',)):
                    if mode=='local':page.locator('#acceleration-select').select_option(accel)
                    for grammar in (False,True):
                        page.evaluate('(v)=>{document.querySelector("#grammar-toggle").checked=v;document.querySelector("#gec-model-section").style.display=v?"":"none"}',grammar)
                        for sync in ('out','waiting','in'):
                            page.evaluate('''s=>{for(const [id,key]of [['sync-signed-out','out'],['sync-waiting','waiting'],['sync-signed-in','in']])document.getElementById(id).style.display=s===key?'':'none';document.getElementById('sync-user-name').textContent='Example User';document.getElementById('sync-user-email').textContent='exa***@example.com';document.getElementById('tab-settings').scrollTop=0;}''',sync)
                            capture(f'{theme}:settings:{mode}:{accel}:{int(grammar)}:{sync}')
            page.evaluate("document.getElementById('tab-settings').scrollTop=10000")
            page.screenshot(path=str(args.output/f'{theme}-settings-bottom.png'))
            page.locator('[data-tab=home]').click()
            if args.platform == 'macos':page.evaluate("document.getElementById('titlebar-close').click()")
            else:page.locator('#titlebar-close').click()
            capture(f'{theme}:modal')
            page.locator('#modal-cancel').click()
            # Setup guide: one scene per step, plus the chosen engine where it changes the step.
            page.evaluate("document.querySelector('.app').classList.add('onboarding-open')")
            for step in ('model','install','microphone','hotkey','try','features'):
                for engine in (('groq','azure-mai','local') if step in ('model','install') else ('',)):
                    page.evaluate('''([step,engine])=>{for(const s of document.querySelectorAll('.ob-step'))s.classList.toggle('active',s.dataset.step===step);for(const c of document.querySelectorAll('.ob-card'))c.classList.toggle('selected',c.dataset.engine===engine);for(const p of document.querySelectorAll('.ob-install'))p.classList.toggle('active',p.dataset.engine===engine);}''',[step,engine])
                    capture(f'{theme}:onboarding:{step}'+(f':{engine}' if engine else ''))
            page.evaluate("document.querySelector('.app').classList.remove('onboarding-open')")
            entries=[{'index':i+1,'start':i*3.0,'end':i*3.0+2.5,'text':text} for i,text in enumerate(['Welcome to Annotate.','This is a native Windows transcription app.','Vulkan runs the local model on your GPU.'])]
            srt='\n\n'.join(f"{e['index']}\n00:00:{int(e['start']):02d},000 --> 00:00:{int(e['end']):02d},500\n{e['text']}" for e in entries)
            page.locator('[data-tab=subtitling]').click()
            page.evaluate('''([entries,srt])=>{for(const id of ['sub-desc','sub-file-zone','sub-lang-group','sub-deps','sub-generate-btn','sub-progress'])document.getElementById(id).style.display='none';document.getElementById('sub-result').style.display='';document.getElementById('sub-result-title').textContent='example';document.getElementById('sub-preview-count').textContent=entries.length+' entries';document.getElementById('sub-preview').textContent=srt;document.getElementById('sub-summarize-btn').disabled=false;}''',[entries,srt])
            for summary in ('','Annotate transcribes speech using a native Windows interface. It supports cloud services and a local Vulkan model.'):
                page.evaluate("s=>{document.getElementById('sub-summary').style.display=s?'':'none';document.getElementById('sub-summary-text').textContent=s;}",summary)
                capture(f'{theme}:subtitling-'+('summary' if summary else 'result'))
            page.evaluate("() => {document.getElementById('sub-summary-text').textContent='A long summary must not push the actions out of reach. '.repeat(14);document.getElementById('tab-subtitling').scrollTop=10000;}")
            capture(f'{theme}:subtitling-long-summary')
            stress = args.output / 'stress'
            stress.mkdir(exist_ok=True)
            large_srt = '\n\n'.join(f"{i}\n{(i-1)*3//3600:02d}:{(i-1)*3//60%60:02d}:{(i-1)*3%60:02d},000 --> {((i-1)*3+2)//3600:02d}:{((i-1)*3+2)//60%60:02d}:{((i-1)*3+2)%60:02d},500\nEntry {i}: subtitles wrap while scrolling through a long transcript.\nUnicode: 你好 日本語 café 😊" for i in range(1, 3001))
            page.evaluate("s=>{document.getElementById('sub-summary').style.display='none';document.getElementById('tab-subtitling').scrollTop=0;document.getElementById('sub-result-title').textContent='large-subtitles';document.getElementById('sub-preview-count').textContent='3000 entries';document.getElementById('sub-preview').textContent=s;}", large_srt)
            for position, offset in [('top', 0), ('bottom', 1e9)]:
                page.evaluate("y=>document.getElementById('sub-preview').scrollTop=y", offset)
                page.screenshot(path=str(stress / f'{theme}-{round(args.scale*96)}-{position}.png'))
            page.evaluate("() => {for(const id of ['sub-desc','sub-file-zone','sub-lang-group','sub-deps','sub-generate-btn'])document.getElementById(id).style.display='';for(const id of ['sub-result','sub-summary'])document.getElementById(id).style.display='none';}")
            for ready in (False,True):
                page.evaluate("v=>{document.getElementById('sub-file-zone').classList.add('has-file');document.getElementById('sub-file-name').textContent='example.wav';document.getElementById('sub-deps').style.display=v?'none':'';document.getElementById('sub-generate-btn').disabled=!v;}",ready)
                capture(f'{theme}:subtitling-'+('ready' if ready else 'file'))
            page.evaluate("() => {document.getElementById('sub-file-zone').classList.remove('has-file');document.getElementById('sub-file-name').textContent='';document.getElementById('sub-deps').style.display='';document.getElementById('sub-generate-btn').disabled=true;}")
            for tab,values in [('dictionary',['Annotate','résumé','Vulkan']),('home',['A short transcription.','This longer transcription demonstrates wrapping across several lines while retaining the original card padding and spacing.','Native recording stays available when the settings window is closed.'])]:
                page.locator(f'[data-tab={tab}]').click()
                now=1700000000000
                page.evaluate('''([tab,values,time])=>{const dict=tab==='dictionary',list=document.getElementById(dict?'dict-list':'history-list');list.innerHTML='';document.getElementById(dict?'dict-count':'history-count').textContent=values.length;if(!dict)document.getElementById('history-clear').style.display='';for(const text of values){const li=document.createElement('li');li.className=dict?'dict-item':'history-item';if(!dict){const t=document.createElement('span');t.className='history-item-time';t.textContent=new Date(time).toLocaleDateString([],{month:'short',day:'numeric'})+' '+new Date(time).toLocaleTimeString([],{hour:'2-digit',minute:'2-digit'});li.append(t);}const s=document.createElement('span');s.className=dict?'dict-item-text':'history-item-text';s.textContent=text;li.append(s);if(dict){const b=document.createElement('button');b.className='dict-item-remove';b.textContent='Remove';li.append(b);}else{const a=document.createElement('span');a.className='history-item-actions';for(const action of ['Copy','Delete']){const b=document.createElement('button');b.className='history-item-'+action.toLowerCase().replace('delete','remove');b.textContent=action;a.append(b);}li.append(a);}list.append(li);}}''',[tab,values,now])
                capture(f'{theme}:{tab}-filled')
                page.evaluate("tab=>{const dict=tab==='dictionary';document.getElementById(dict?'dict-list':'history-list').innerHTML='<li class=\"'+(dict?'dict-empty':'history-empty')+'\">'+(dict?'No terms added yet':'No transcriptions yet')+'</li>';document.getElementById(dict?'dict-count':'history-count').textContent='';if(!dict)document.getElementById('history-clear').style.display='none';}",tab)
        browser.close()
    print(f'Captured reference screenshots in {args.output}')

if __name__=='__main__':main()
