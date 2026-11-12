// Run via: PLAYWRIGHT_BROWSERS_PATH=... FORGEFLOW_PLAYWRIGHT_PATH=... cargo test phone_media_browser_integration -- --ignored
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const { chromium } = require(process.env.FORGEFLOW_PLAYWRIGHT_PATH || 'playwright');
(async () => {
  const browser = await chromium.launch({headless:true});
  const context = await browser.newContext({viewport:{width:390,height:844},hasTouch:true,isMobile:true});
  const page = await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const root=process.argv[3];
  try {
    await page.goto(process.argv[2]);
    assert.equal(await page.locator('details[open]').count(),0);await page.getByRole('link',{name:'Media',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#media-count').textContent==='16 files');
    assert.equal(await page.locator('.media-card').count(),12);
    await page.locator('#next').click();assert.equal(await page.locator('.media-card').count(),4);
    await page.locator('#media-filter').selectOption('image');assert.equal(await page.locator('#media-count').textContent(),'14 files');assert.equal(await page.locator('#page-number').textContent(),'Page 1 of 2');
    const image=page.locator('.media-card img').first();await image.click();assert.equal(await page.locator('#media-viewer').evaluate(e=>e.open),true);await page.locator('#close-viewer').click();assert.equal(await page.locator('#media-viewer').evaluate(e=>e.open),false);
    await image.click();await page.keyboard.press('Escape');assert.equal(await page.locator('#media-viewer').evaluate(e=>e.open),false);
    await page.locator('#media-filter').selectOption('video');assert.equal(await page.locator('.media-card').count(),2);
    await page.waitForFunction(()=>[...document.querySelectorAll('.media-card video')].every(v=>v.muted&&v.autoplay&&v.loop&&v.playsInline&&!v.paused));
    await page.locator('#media-filter').selectOption('all');
    async function card(path){for(let tries=0;tries<4;tries++){const card=page.locator('.media-card').filter({has:page.locator('.filename',{hasText:path})});if(await card.count())return card;if(await page.locator('#next').isDisabled())await page.locator('#previous').click();else await page.locator('#next').click();}throw new Error('missing card '+path);}
    await (await card('.tmp/image-00.png')).getByRole('checkbox').check();
    await (await card('.tmp/clip.webm')).getByRole('checkbox').check();
    await (await card('.tmp/clip.mp4')).getByRole('checkbox').check();
    assert.equal(await page.locator('#selection li').count(),3);
    await page.locator('#selection li').nth(2).getByRole('button',{name:/earlier/}).click();
    assert.match(await page.locator('#selection li').nth(1).textContent(),/clip.mp4/);
    await page.locator('#still-seconds').fill('0.5');await page.locator('#combine').click();
    await page.locator('#media-jobs video').waitFor({timeout:90000});assert.equal(await page.locator('#media-error').textContent(),'');
    await page.getByRole('button',{name:'Refresh',exact:true}).click();await page.waitForFunction(()=>Number.parseInt(document.querySelector('#media-count').textContent)>=17);
    await (await card('.tmp/image-00.png')).getByRole('button',{name:'Use as source'}).click();
    await (await card('.tmp/clip.webm')).getByRole('button',{name:'Use as target'}).click();
    assert.equal(await page.locator('#target-label').evaluate(e=>{const r=e.getBoundingClientRect();return r.top>=0&&r.bottom<=window.innerHeight;}),true,'Choosing another file returns to the face picker even when the hash is unchanged');
    await page.waitForFunction(()=>document.querySelector('#source-picker').hasAttribute('width'));
    // Coordinate-based pointer actions do not auto-scroll like locator actions.
    await page.locator('#source-picker').scrollIntoViewIfNeeded();
    const box=await page.locator('#source-picker').boundingBox();await page.mouse.move(box.x+box.width*0.15,box.y+box.height*0.15);await page.mouse.down();await page.mouse.move(box.x+box.width*0.6,box.y+box.height*0.6);await page.mouse.up();
    await page.locator('#scan-target').click();await page.waitForFunction(()=>document.querySelector('#target-picker').hasAttribute('width'));
    await page.locator('#target-picker').scrollIntoViewIfNeeded();
    const target=await page.locator('#target-picker').boundingBox();await page.mouse.click(target.x+target.width*0.72,target.y+target.height*0.45);assert.match(await page.locator('#chosen-face').textContent(),/face 2/);
    await page.locator('#swap').click();await page.locator('#media-jobs img[src="/files/.tmp/phone-swapped.png"]').waitFor({timeout:30000});assert.equal(await page.locator('#media-error').textContent(),'');
    await page.locator('#media-upload').setInputFiles({name:'from-phone.png',mimeType:'image/png',buffer:await fs.readFile(root+'/.tmp/image-00.png')});await page.getByRole('button',{name:'Upload',exact:true}).click();await page.waitForFunction(()=>Number.parseInt(document.querySelector('#media-count').textContent)>=19);
    for(let i=0;i<60;i++)await fs.copyFile(root+'/.tmp/image-00.png',root+`/.tmp/paging-${String(i).padStart(2,'0')}.png`);
    await page.getByRole('button',{name:'Refresh',exact:true}).click();await page.waitForFunction(()=>Number.parseInt(document.querySelector('#media-count').textContent)>=79);
    await page.locator('#last-page').click();const total=Number.parseInt((await page.locator('#page-number').textContent()).split(' of ')[1]);assert(total>=7);assert.equal(await page.locator('#page-number').textContent(),`Page ${total} of ${total}`);
    await page.locator('#first-page').click();await page.locator('#skip-two').click();assert.equal(await page.locator('#page-number').textContent(),`Page 3 of ${total}`);
    await page.locator('#back-two').click();assert.equal(await page.locator('#page-number').textContent(),`Page 1 of ${total}`);
    await page.locator('#page-input').fill('4');await page.locator('#page-jump').getByRole('button',{name:'Go',exact:true}).click();assert.equal(await page.locator('#page-number').textContent(),`Page 4 of ${total}`);
    await page.getByRole('button',{name:'Go to page 2',exact:true}).click();assert.equal(await page.locator('#page-number').textContent(),`Page 2 of ${total}`);
    const locked=await browser.newContext();const denied=await locked.request.get(new URL('/media/list',process.argv[2]).href);assert.equal(denied.status(),403);await locked.close();
    assert.deepEqual(errors,[]);console.log('Phone integration passed: menu, mixed gallery, paging, client filters, muted autoplay, fullscreen/exit, ordered MP4 job + SSE, explicit source/target + face picker, swap job + SSE, upload, and authentication.');
  } catch (e) {
    console.error('Browser diagnostics:', JSON.stringify({errors, url:page.url(), count:await page.locator('#media-count').textContent().catch(()=>null), error:await page.locator('#media-error').textContent().catch(()=>null)}));
    await page.screenshot({path:'/tmp/forgeflow-phone-media-failure.png',fullPage:true});throw e;
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exit(1);});
