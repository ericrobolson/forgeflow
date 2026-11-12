// Small phone gallery and face-picker controller; no framework or model calls.
(function () {
  const PAGE_SIZE = 12;
  const filtered = (files, kind) => files.filter(f => kind === "all" || (kind === "video" ? f.video : !f.video));
  const pageItems = (files, page) => files.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE);
  const rectangle = (a, b) => ({x: Math.min(a.x,b.x), y: Math.min(a.y,b.y), width: Math.abs(a.x-b.x), height: Math.abs(a.y-b.y)});
  const faceAt = (faces, x, y) => faces.findIndex(([l,t,r,b]) => x >= l && x <= r && y >= t && y <= b);
  if (typeof module !== "undefined") module.exports = {filtered, pageItems, rectangle, faceAt};
  if (typeof document === "undefined") return;
  const $ = id => document.getElementById(id);
  let files = [], selected = [], page = 0, source = null, target = null, sourceImage = null, sourceBox = null, scan = null, targetImage = null, chosenFace = null;
  const error = message => { $("media-error").textContent = message || ""; };
  const fileUrl = path => "/files/" + path.split("/").map(encodeURIComponent).join("/");
  const button = (text, action) => {const b=document.createElement("button");b.type="button";b.textContent=text;b.addEventListener("click",action);return b;};
  async function request(url, body, html=false) {
    const response = await fetch(url, body ? {method:"POST",body} : {});
    if (!response.ok) {const data=await response.json();throw new Error(data.error || "Request failed");}
    return html ? response.text() : response.json();
  }
  async function refresh() {
    try {const latest=(await request("/media/list")).files;if(JSON.stringify(files)===JSON.stringify(latest)&&$("media-grid").children.length)return;files=latest;selected=selected.filter(path=>files.some(f=>f.path===path));render();renderSelection();}catch(e){error(e.message);}
  }
  function openViewer(file) {
    const content=$("viewer-content");content.replaceChildren();
    const media=document.createElement(file.video ? "video" : "img");media.src=fileUrl(file.path);
    if(file.video){media.controls=true;media.muted=true;media.autoplay=true;media.loop=true;media.playsInline=true;}else{media.alt=file.path;}
    content.append(media);$("media-viewer").showModal();
  }
  function closeViewer() {$("media-viewer").close();$("viewer-content").replaceChildren();}
  $("close-viewer").addEventListener("click",closeViewer);
  $("media-viewer").addEventListener("click",e=>{if(e.target===$("media-viewer")||e.target===$("viewer-content"))closeViewer();});
  $("media-viewer").addEventListener("close",()=>$("viewer-content").replaceChildren());
  function render() {
    const visible=filtered(files,$("media-filter").value), pages=Math.max(1,Math.ceil(visible.length/PAGE_SIZE));page=Math.max(0,Math.min(page,pages-1));
    $("page-number").textContent=`Page ${page+1} of ${pages}`;$("previous").disabled=page===0;$("next").disabled=page===pages-1;
    $("first-page").disabled=page===0;$("back-two").disabled=page===0;$("last-page").disabled=page===pages-1;$("skip-two").disabled=page===pages-1;
    $("page-input").max=String(pages);$("page-input").value=String(page+1);const links=$("page-links");links.replaceChildren();
    const start=Math.max(0,Math.min(page-2,pages-5));for(let n=start;n<Math.min(pages,start+5);n++){const b=button(String(n+1),()=>{page=n;render();});b.setAttribute("aria-label",`Go to page ${n+1}`);if(n===page){b.setAttribute("aria-current","page");b.disabled=true;}links.append(b);}

    $("media-count").textContent=`${visible.length} files`;
    const grid=$("media-grid");grid.replaceChildren();
    for(const file of pageItems(visible,page)) {
      const card=document.createElement("article");card.className="media-card";
      const preview=document.createElement(file.video ? "video" : "img");preview.src=fileUrl(file.path);
      if(file.video){preview.muted=true;preview.defaultMuted=true;preview.autoplay=true;preview.loop=true;preview.playsInline=true;preview.preload="metadata";}else{preview.alt=file.path;preview.loading="lazy";}
      preview.addEventListener("click",()=>openViewer(file));card.append(preview);
      const name=document.createElement("span");name.className="filename";name.textContent=file.path;card.append(name);
      const label=document.createElement("label"),check=document.createElement("input");check.type="checkbox";check.checked=selected.includes(file.path);check.setAttribute("aria-label",`Select ${file.path}`);
      check.addEventListener("change",()=>{if(check.checked)selected.push(file.path);else selected=selected.filter(p=>p!==file.path);renderSelection();});label.append(check," Select");card.append(label);
      if(!file.video)card.append(button("Use as source",()=>chooseSource(file)));
      card.append(button("Use as target",()=>chooseTarget(file)));grid.append(card);
    }
    if(!visible.length){const p=document.createElement("p");p.textContent="No matching media yet. Upload files or generate a scene.";grid.append(p);}
  }
  function renderSelection(){
    const list=$("selection");list.replaceChildren();
    selected.forEach((path,index)=>{const li=document.createElement("li");li.append(document.createTextNode(path+" "));
      const move=delta=>{const next=index+delta;if(next<0||next>=selected.length)return;[selected[index],selected[next]]=[selected[next],selected[index]];renderSelection();};
      const up=button("↑",()=>move(-1));up.setAttribute("aria-label",`Move ${path} earlier`);up.disabled=index===0;
      const down=button("↓",()=>move(1));down.setAttribute("aria-label",`Move ${path} later`);down.disabled=index===selected.length-1;
      li.append(up,down,button("Remove",()=>{selected.splice(index,1);render();renderSelection();}));list.append(li);
    });$("combine").disabled=selected.length<2;
  }
  function resetTarget(){scan=null;chosenFace=null;targetImage=null;$("target-picker").removeAttribute("width");$("chosen-face").textContent="No target face selected.";}
  function showFaceSwap(){location.hash="face-swap";$("face-swap").scrollIntoView({block:"start"});}
  function chooseSource(file){source=file.path;sourceBox=null;sourceImage=null;$("source-label").textContent=`Source: ${source}`;const image=new Image();image.onload=()=>{if(source!==file.path)return;sourceImage=image;drawSource();};image.src=fileUrl(source);showFaceSwap();}
  function chooseTarget(file){target=file.path;$("target-label").textContent=`Target: ${target}`;resetTarget();showFaceSwap();}
  function drawSource(){if(!sourceImage)return;const canvas=$("source-picker");canvas.width=sourceImage.naturalWidth;canvas.height=sourceImage.naturalHeight;const ctx=canvas.getContext("2d");ctx.drawImage(sourceImage,0,0);if(sourceBox){ctx.strokeStyle="#00ff88";ctx.lineWidth=Math.max(2,canvas.width/200);ctx.strokeRect(sourceBox.x*canvas.width,sourceBox.y*canvas.height,sourceBox.width*canvas.width,sourceBox.height*canvas.height);}}
  const position=(canvas,event)=>{const rect=canvas.getBoundingClientRect();return {x:Math.max(0,Math.min(1,(event.clientX-rect.left)/rect.width)),y:Math.max(0,Math.min(1,(event.clientY-rect.top)/rect.height))};};
  let anchor=null;const sourceCanvas=$("source-picker");
  sourceCanvas.addEventListener("pointerdown",e=>{if(!sourceImage)return;anchor=position(sourceCanvas,e);sourceCanvas.setPointerCapture(e.pointerId);});
  sourceCanvas.addEventListener("pointermove",e=>{if(anchor){sourceBox=rectangle(anchor,position(sourceCanvas,e));drawSource();}});
  sourceCanvas.addEventListener("pointerup",e=>{if(anchor){sourceBox=rectangle(anchor,position(sourceCanvas,e));anchor=null;drawSource();}});
  sourceCanvas.addEventListener("pointercancel",()=>{anchor=null;sourceBox=null;drawSource();});
  $("reset-source").addEventListener("click",()=>{sourceBox=null;drawSource();});
  function drawTarget(){if(!targetImage||!scan)return;const canvas=$("target-picker");canvas.width=targetImage.naturalWidth;canvas.height=targetImage.naturalHeight;const ctx=canvas.getContext("2d");ctx.drawImage(targetImage,0,0);ctx.lineWidth=Math.max(2,canvas.width/200);ctx.font=`${Math.max(16,canvas.width/35)}px sans-serif`;
    scan.faces.forEach(([l,t,r,b],index)=>{ctx.strokeStyle=index===chosenFace?"#00ff88":"#ffb000";ctx.fillStyle=ctx.strokeStyle;ctx.strokeRect(l,t,r-l,b-t);ctx.fillText(String(index+1),l,Math.max(20,t));});}
  $("target-picker").addEventListener("pointerup",e=>{if(!scan)return;const p=position($("target-picker"),e),index=faceAt(scan.faces,p.x*scan.width,p.y*scan.height);if(index<0)return;chosenFace=index;$("chosen-face").textContent=`Target face ${index+1} selected.`;drawTarget();});
  $("scan-target").addEventListener("click",async()=>{
    if(!source||!target){error("Explicitly choose a source image and a target first.");return;}error("");resetTarget();const requested=target;
    $("scan-target").disabled=true;$("scan-status").textContent="Finding faces and preparing the target frame…";
    try{const body=new FormData();body.append("target",target);const result=await request("/media/scan",body);if(target!==requested)return;scan=result;const image=new Image();image.onload=()=>{if(scan!==result)return;targetImage=image;drawTarget();};image.src=fileUrl(scan.preview);$("scan-status").textContent=`${scan.faces.length} faces found. Tap the face to replace.`;}catch(e){error(e.message);$("scan-status").textContent="Face scan failed.";}finally{$("scan-target").disabled=false;}
  });
  async function startJob(url,body){error("");try{const markup=await request(url,body,true);const container=document.createElement("div");container.innerHTML=markup;$("media-jobs").prepend(container);htmx.process(container);container.scrollIntoView({behavior:"smooth",block:"start"});}catch(e){error(e.message);}}
  $("combine").addEventListener("click",()=>{const body=new FormData();selected.forEach(p=>body.append("path",p));body.append("seconds",$("still-seconds").value);startJob("/media/combine",body);});
  $("swap").addEventListener("click",()=>{if(!source||!target||!sourceBox||sourceBox.width<=0||sourceBox.height<=0||!scan||scan.target!==target||chosenFace===null){error("Choose source and target, box the source face, and tap a detected target face.");return;}const body=new FormData();body.append("source",source);body.append("scan",scan.id);body.append("rect",JSON.stringify(sourceBox));body.append("face",String(chosenFace));startJob("/media/swap",body);});
  $("upload-form").addEventListener("submit",async e=>{e.preventDefault();const input=$("media-upload");if(!input.files.length)return;error("");const body=new FormData();for(const file of input.files)body.append(file.name,file);const submit=e.currentTarget.querySelector("button");submit.disabled=true;try{await request("/media/upload",body);input.value="";await refresh();}catch(e){error(e.message);}finally{submit.disabled=false;}});
  $("media-filter").addEventListener("change",()=>{page=0;render();});$("previous").addEventListener("click",()=>{page--;render();});$("next").addEventListener("click",()=>{page++;render();});
  $("first-page").addEventListener("click",()=>{page=0;render();});$("last-page").addEventListener("click",()=>{page=Math.ceil(filtered(files,$("media-filter").value).length/PAGE_SIZE)-1;render();});
  $("back-two").addEventListener("click",()=>{page-=2;render();});$("skip-two").addEventListener("click",()=>{page+=2;render();});
  $("page-jump").addEventListener("submit",event=>{event.preventDefault();const n=Number($("page-input").value);if(Number.isInteger(n)&&n>=1){page=n-1;render();}});
  document.querySelectorAll("[data-refresh]").forEach(b=>b.addEventListener("click",refresh));
  setInterval(()=>{if(!document.hidden)refresh();},5000);refresh();
})();
