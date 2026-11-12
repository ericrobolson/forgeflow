// Real HTTP/browser integration with a scripted OpenAI-compatible endpoint; no model download needed.
const assert=require('node:assert/strict'),fs=require('node:fs/promises'),path=require('node:path'),os=require('node:os'),http=require('node:http'),net=require('node:net');
const {spawn}=require('node:child_process');
const {chromium}=require(process.env.FORGEFLOW_PLAYWRIGHT_PATH||'playwright');
(async()=>{
 let calls=0;const requests=[];
 const api=http.createServer(async(req,res)=>{let raw='';for await(const chunk of req)raw+=chunk;const body=JSON.parse(raw);calls++;requests.push(body);const archive=body.messages[0].content.includes('List facts implied by narration');
  const text=archive?'\u007b"facts":[]\u007d':'Gareth lowers his lantern and lets you pass.';res.writeHead(200,{'Content-Type':'text/event-stream'});res.end('data: '+JSON.stringify({choices:[{delta:{content:text},finish_reason:null}]})+'\n\ndata: [DONE]\n\n');
 });await new Promise(resolve=>api.listen(0,'127.0.0.1',resolve));
 const root=await fs.mkdtemp(path.join(os.tmpdir(),'forgeflow-roleplay-phone-'));await fs.mkdir(path.join(root,'_forgeflow'));await fs.writeFile(path.join(root,'_forgeflow/config.json'),JSON.stringify({endpoint:`http://127.0.0.1:${api.address().port}/v1`,endpoint_model:'roleplay-test'}));
 const portServer=net.createServer();await new Promise(resolve=>portServer.listen(0,'127.0.0.1',resolve));const port=portServer.address().port;await new Promise(resolve=>portServer.close(resolve));
 const child=spawn(path.resolve(process.argv[2]||'target/debug/forgeflow'),['serve',String(port)],{cwd:root,env:{...process.env,FORGEFLOW_NO_UPDATE_CHECK:'1'},stdio:['pipe','pipe','pipe']});let stderr='';child.stderr.on('data',data=>stderr+=data);
 const browser=await chromium.launch({headless:true});
 try{
  const url=await new Promise((resolve,reject)=>{let output='';const timer=setTimeout(()=>reject(new Error('Startup timeout: '+stderr)),10000);child.stdout.on('data',data=>{output+=data;const m=output.match(/http:\/\/localhost:\d+\/\?t=\w+/);if(m){clearTimeout(timer);resolve(m[0]);}});child.on('exit',code=>{clearTimeout(timer);reject(new Error('Server exited '+code+': '+stderr));});});
  const context=await browser.newContext({viewport:{width:320,height:844},hasTouch:true,isMobile:true});const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto(url);const roleplay=page.getByRole('link',{name:'Roleplay',exact:true});await roleplay.waitFor({state:'visible'});const bounds=await roleplay.boundingBox();assert(bounds.x>=0&&bounds.x+bounds.width<=320);await roleplay.click();assert.equal(calls,0);
  await page.getByLabel('Name',{exact:true}).fill('Phone Adventure');await page.getByRole('button',{name:'Create session',exact:true}).click();await page.waitForURL('**/roleplay/play');assert.equal(calls,0);assert.match(await page.locator('#log').textContent(),/The Bridge at Midnight/);
  assert.equal(await page.locator('#composer').getAttribute('hx-post'),'/roleplay/send');await page.locator('#message').fill('I wait');await page.locator('#composer .send').click();await page.getByText('Gareth lowers his lantern and lets you pass.',{exact:true}).waitFor();assert.equal(calls,2);assert(requests[0].messages[0].content.includes('## Scene'));
  await page.getByRole('link',{name:'Roleplay',exact:true}).click();await page.getByRole('button',{name:'Close session',exact:true}).click();await page.waitForURL('**/roleplay');assert.equal(calls,2);
  await page.getByRole('button',{name:'Resume PhoneAdventure',exact:true}).click();await page.waitForURL('**/roleplay/play');assert.match(await page.locator('#log').textContent(),/Gareth lowers his lantern and lets you pass\./);assert.equal(calls,2);
  await page.getByRole('link',{name:'Roleplay',exact:true}).click();await page.getByLabel('Name',{exact:true}).fill('Blank Adventure');await page.locator('select[name=seed]').selectOption('blank');await page.getByRole('button',{name:'Create session',exact:true}).click();await page.waitForURL('**/roleplay/play');assert.match(await page.locator('#log').textContent(),/No scene has been established/);assert.equal(calls,2);
  await page.getByRole('link',{name:'Chat',exact:true}).click();await page.waitForURL(`http://localhost:${port}/`);await page.locator('input[name=mode][value=chat]').waitFor({state:'attached'});assert.deepEqual(errors,[]);
  const anonymous=await browser.newContext();const denied=await anonymous.request.get(`http://localhost:${port}/roleplay`);assert.equal(denied.status(),403);await anonymous.close();
  console.log('Roleplay phone integration passed: visible header, sample + blank creation, streamed turn, close/resume, deterministic recap without new model calls, return to chat, and authentication.');
 }finally{await browser.close();if(child.exitCode===null&&child.signalCode===null){const exit=new Promise(resolve=>child.once('exit',resolve));child.kill('SIGTERM');await exit;}await new Promise(resolve=>api.close(resolve));await fs.rm(root,{recursive:true,force:true});}
})().catch(e=>{console.error(e);process.exit(1);});
