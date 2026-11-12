// Integration check against both real CLI servers: node tests/phone_navigation.cjs [forgeflow binary]
const assert=require('node:assert/strict');
const fs=require('node:fs/promises');
const path=require('node:path');
const os=require('node:os');
const net=require('node:net');
const {spawn,spawnSync}=require('node:child_process');
const {chromium}=require(process.env.FORGEFLOW_PLAYWRIGHT_PATH||'playwright');
const binary=path.resolve(process.argv[2]||'target/debug/forgeflow');
(async()=>{
 const browser=await chromium.launch({headless:true});
 try{
  for(const rpg of [false,true]){
   const root=await fs.mkdtemp(path.join(os.tmpdir(),'forgeflow-phone-nav-'));await fs.mkdir(path.join(root,'_forgeflow'));await fs.writeFile(path.join(root,'_forgeflow/config.json'),JSON.stringify({endpoint:'http://127.0.0.1:1/v1',endpoint_model:'navigation-test'}));await fs.mkdir(path.join(root,'.tmp'));
   const server=net.createServer();await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const port=server.address().port;await new Promise(resolve=>server.close(resolve));
   for(const color of ['red','blue']){const r=spawnSync('ffmpeg',['-v','error','-y','-f','lavfi','-i',`color=c=${color}:s=80x60`,'-frames:v','1',path.join(root,'.tmp',`${color}.png`)]);assert.equal(r.status,0,String(r.stderr));}
   const child=spawn(binary,rpg?['rpg','serve',String(port)]:['serve',String(port)],{cwd:root,env:{...process.env,FORGEFLOW_NO_UPDATE_CHECK:'1'},stdio:['pipe','pipe','pipe']});let stderr='';child.stderr.on('data',data=>stderr+=data);
   const url=new Promise((resolve,reject)=>{let output='';const timer=setTimeout(()=>reject(new Error('Server startup timed out: '+stderr)),10000);child.stdout.on('data',data=>{output+=data;const match=output.match(/http:\/\/localhost:\d+\/\?t=\w+/);if(match){clearTimeout(timer);resolve(match[0]);}});child.on('exit',code=>{clearTimeout(timer);reject(new Error('Server exited '+code+': '+stderr));});});
   if(rpg)child.stdin.write('n\nNavigation With An Intentionally Long Name\ns\n');
   try{
    const address=await url;
    for(const width of [320,390,430]){
     const context=await browser.newContext({viewport:{width,height:844},hasTouch:true,isMobile:true});const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
     await page.goto(address);const media=page.getByRole('link',{name:'Media',exact:true});await media.waitFor({state:'visible'});assert.equal(await page.locator('details[open]').count(),0);
     const bounds=await media.boundingBox();assert(bounds.x>=0&&bounds.x+bounds.width<=width,`Media outside ${width}px viewport`);await media.click();await page.waitForFunction(()=>document.getElementById('media-count').textContent.startsWith('2 files')||Number.parseInt(document.getElementById('media-count').textContent)>2);
     if(width===320){await fs.copyFile(path.join(root,'.tmp/red.png'),path.join(root,'.tmp/manually-saved.png'));await page.reload();await page.waitForFunction(()=>document.getElementById('media-count').textContent==='3 files');assert.equal(await page.locator('.filename',{hasText:'manually-saved.png'}).count(),1);await page.locator('.media-card input[type=checkbox]').nth(0).check();await page.locator('.media-card input[type=checkbox]').nth(1).check();await page.locator('#still-seconds').fill('0.5');await page.locator('#combine').click();await page.locator('#media-jobs video').waitFor({timeout:30000});assert.equal(await page.locator('#media-error').textContent(),'');}
     assert.deepEqual(errors,[]);await context.close();
    }
    console.log(`${rpg?'RPG':'Chat'}: Media visible without opening a menu at 320/390/430px; gallery and real combine/SSE job passed.`);
   }finally{if(child.exitCode===null&&child.signalCode===null){const exited=new Promise(resolve=>child.once('exit',resolve));child.kill('SIGTERM');await exited;}await fs.rm(root,{recursive:true,force:true});}
  }
 }finally{await browser.close();}
})().catch(e=>{console.error(e);process.exit(1);});
