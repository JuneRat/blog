"use strict";
const $ = id => document.getElementById(id);
let csrf = "", current = null, selected = null, restoreKey = "", validated = false, formsLoaded = false;
const completed = new Set();
const callbacks = new Map();
const phases = {queued:"等待处理",checking:"正在检查",snapshot:"创建恢复前副本",database:"恢复数据库",files:"恢复媒体和主题",configuration:"恢复站点配置",verifying:"验证恢复结果",backup:"生成加密备份",uploading:"上传远程存储",complete:"完成",failed:"失败"};
const kinds = {backup:"备份",restore:"恢复",inspect:"备份验证","remote-save":"远程连接测试","remote-list":"读取远程备份","remote-upload":"上传远程备份","remote-download":"取回远程备份"};
const states = {running:"进行中",succeeded:"已完成",failed:"未完成",interrupted:"已中断"};
const formatDate = value => typeof value === "number" ? new Date(value * 1000).toLocaleString() : new Date(value).toLocaleString();
const size = value => value >= 1024**3 ? `${(value/1024**3).toFixed(2)} GiB` : value >= 1024**2 ? `${(value/1024**2).toFixed(1)} MiB` : `${(value/1024).toFixed(1)} KiB`;
function message(text, error=false) { $("message").textContent=text; $("message").hidden=!text; $("message").classList.toggle("error",error); }
function invalidate() { validated=false; $("restore-confirmation").hidden=true; $("confirm-restore").value=""; }
function selectBackup(value) { selected=value; $("discard-upload").hidden=!value.imported; invalidate(); $("selected-backup").textContent=`已选择：${value.label || value.name}`; }
async function fileText(input) { const file=input.files[0]; if(!file) throw new Error("请选择恢复密钥文件"); if(file.size>16384) throw new Error("恢复密钥文件过大"); return file.text(); }
async function api(path, options={}) {
  const headers={...options.headers}; if(csrf) headers["X-CSRF-Token"]=csrf;
  const response=await fetch(path,{...options,headers,cache:"no-store"});
  const value=response.status===204?{}:await response.json();
  if(!response.ok) { if(response.status===401) { $("login-panel").hidden=false; $("workspace").hidden=true; } throw new Error(value.error || "操作未完成，请重试"); }
  return value;
}
async function action(name,input={},callback) {
  const result=await api("/api/recovery/action",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({action:name,input})});
  if(result.job_id) { if(callback) callbacks.set(result.job_id,callback); message("任务已提交，关闭页面也会继续运行。"); await refresh(); }
  else { if(callback) await callback(result); await refresh(); }
  return result;
}
function safeHandler(fn) { return async event => { if(event) event.preventDefault(); const button=event?.submitter || (event?.currentTarget?.tagName==="BUTTON"?event.currentTarget:null); if(button) button.disabled=true; try { await fn(event); } catch(error) { message(error instanceof TypeError?"连接暂时中断。已提交的任务会继续运行，请稍后刷新查看。":error.message,true); } finally { if(button) button.disabled=false; } }; }
function button(label,fn,danger=false) { const element=document.createElement("button"); element.type="button"; element.textContent=label; element.className=danger?"quiet":"secondary"; element.addEventListener("click",safeHandler(fn)); return element; }
function cell(row,text) { const element=document.createElement("td"); element.textContent=text; row.append(element); return element; }
function renderBackups(backups) {
  $("backups").replaceChildren(); $("backup-empty").hidden=backups.length>0; $("backup-table").hidden=backups.length===0;
  for(const backup of backups) {
    const row=document.createElement("tr"); cell(row,formatDate(backup.created_at)); cell(row,size(backup.size)); cell(row,backup.protected?"恢复前副本":"常规备份");
    const operations=cell(row,""); const actions=document.createElement("div"); actions.className="actions";
    actions.append(button("下载",()=>{location.href=`/api/recovery/download/${encodeURIComponent(backup.name)}`;}),button("恢复",()=>{selectBackup(backup);$("selected-backup").scrollIntoView({behavior:"smooth",block:"center"});}),button("删除",async()=>{if(confirm(`删除 ${formatDate(backup.created_at)} 的${backup.protected?"恢复前副本":"备份"}？`)) await action("delete",{name:backup.name});},true));
    if(current.settings.remote) actions.append(button("重传远程",()=>action("remote-upload",{name:backup.name})));
    operations.append(actions); $("backups").append(row);
  }
}
function loadForms(settings) {
  $("frequency").value=settings.schedule || "off"; $("backup-hour").value=settings.hour_utc ?? 18; $("weekday").value=settings.weekday || 0; $("keep").value=settings.keep || 7; $("remote-keep").value=settings.remote_keep || 30;
  const remote=settings.remote; if(remote) for(const key of ["endpoint","region","bucket","prefix"]) $(key).value=remote[key];
  $("access-key").value=""; $("secret-key").value="";
}
function renderJobs(jobs) {
  $("jobs").replaceChildren(); $("job-empty").hidden=jobs.length>0;
  for(const job of jobs.slice(0,20)) {
    const item=document.createElement("li"),heading=document.createElement("div"),title=document.createElement("strong"),state=document.createElement("span"),detail=document.createElement("p");
    heading.className="job-heading"; title.textContent=`${kinds[job.kind] || "任务"} · ${formatDate(job.started_at)}`; state.textContent=states[job.status] || job.status; state.className=`job-state ${job.status}`;
    heading.append(title,state); detail.className="job-detail"; detail.textContent=job.status==="running"?(phases[job.phase] || "正在处理"):job.message || "操作完成";
    item.append(heading,detail); if(job.rollback) item.append(button("选择恢复前副本回滚",()=>{selectBackup({name:job.rollback,imported:false});$("selected-backup").scrollIntoView({behavior:"smooth"});}));
    if(job.result?.needs_activation && current.maintenance) { const note=document.createElement("p");note.className="job-detail";note.textContent="数据已恢复，等待站点重新启动。";item.append(note); }
    $("jobs").append(item);
    if(job.status!=="running" && !completed.has(job.id)) {
      completed.add(job.id); const callback=callbacks.get(job.id); callbacks.delete(job.id);
      if(callback) { if(job.status==="succeeded") Promise.resolve(callback(job.result)).catch(error=>message(error.message,true)); else message(job.message || "任务未完成，请重试",true); }
    }
  }
}
async function refresh() {
  const value=await api("/api/recovery/session"); current=value; csrf=value.csrf;
  $("login-panel").hidden=true; $("workspace").hidden=false; $("logout").hidden=false;
  $("key-setup").hidden=value.initialized || value.installing;
  $("backup-now").disabled=!value.initialized || value.busy || value.maintenance || value.installing;
  $("maintenance").hidden=!value.maintenance;
  $("maintenance-text").textContent=value.recovery_required?"恢复尚未完成。请重试原备份，或选择任务记录中的恢复前副本回滚。数据校验完成后可重新启动站点。":"任务完成后会自动重新开放访问。若数据库故障，请修复数据库服务后重新启动站点。";
  $("resume").disabled=value.busy; $("restore-database").hidden=value.database_configured;
  $("remote-list").disabled=!value.settings.remote || value.busy;
  $("disable-remote").disabled=!value.settings.remote || value.busy;
  $("next-run").textContent=value.settings.next_run?`下次运行：${formatDate(value.settings.next_run)}`:"自动备份已关闭";
  $("refresh-state").textContent=value.busy?"任务执行中 · 每 3 秒更新":"自动更新";
  if(!formsLoaded) {loadForms(value.settings);formsLoaded=true;}
  renderBackups(value.backups);renderJobs(value.jobs);
}
$("login-method").addEventListener("change",()=>{const mode=$("login-method").value;$("password-fields").hidden=mode!=="password";$("key-fields").hidden=mode!=="key";$("installation-fields").hidden=mode!=="installation";});
$("login-form").addEventListener("submit",safeHandler(async()=>{
  const mode=$("login-method").value,input={};
  if(mode==="password") {input.username=$("username").value;input.password=$("password").value;}
  if(mode==="key") {input.key=await fileText($("login-key"));restoreKey=input.key;}
  if(mode==="installation") input.installation_token=$("installation-token").value;
  const result=await api("/api/recovery/session",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(input)});
  csrf=result.csrf;$("password").value="";$("installation-token").value="";message("");await refresh();
}));
$("logout").addEventListener("click",safeHandler(async()=>{await api("/api/recovery/session",{method:"DELETE"});location.reload();}));
$("generate-key").addEventListener("click",safeHandler(async()=>{await action("keygen",{},result=>{const url=URL.createObjectURL(new Blob([result.key],{type:"application/json"}));const a=document.createElement("a");a.href=url;a.download="blog-recovery-key.json";a.click();setTimeout(()=>URL.revokeObjectURL(url),10000);$("key-confirmation").hidden=false;message("恢复密钥已下载。请重新选择该文件，确认已妥善保存。");});}));
$("confirm-key-button").addEventListener("click",safeHandler(async()=>{const key=await fileText($("confirm-key"));await action("key-confirm",{key});restoreKey=key;message("恢复密钥已确认，可以开始备份。");}));
$("backup-now").addEventListener("click",safeHandler(()=>action("backup",{},()=>message("备份已完成，可在列表中下载。"))));
$("resume").addEventListener("click",safeHandler(()=>action("resume",{},()=>message("站点已重新启动。"))));
for(let hour=0;hour<24;hour++) {const option=document.createElement("option");option.value=String(hour);option.textContent=String(hour).padStart(2,"0")+":00 UTC";$("backup-hour").append(option);}
$("schedule-form").addEventListener("submit",safeHandler(()=>action("schedule",{settings:{schedule:$("frequency").value,hour_utc:Number($("backup-hour").value),weekday:Number($("weekday").value),keep:Number($("keep").value),remote_keep:Number($("remote-keep").value)}},()=>message("自动备份设置已保存。"))));
$("remote-form").addEventListener("submit",safeHandler(()=>action("remote-save",{remote:{endpoint:$("endpoint").value,region:$("region").value,bucket:$("bucket").value,prefix:$("prefix").value,access_key:$("access-key").value,secret_key:$("secret-key").value}},()=>{formsLoaded=false;message("远程读写测试通过，设置已保存。");})));
$("disable-remote").addEventListener("click",safeHandler(async()=>{if(confirm("停用远程存储？已有远程备份会保留。")) await action("remote-save",{remote:null},()=>message("远程存储已停用。"));}));
$("remote-list").addEventListener("click",safeHandler(()=>action("remote-list",{},result=>{
  const container=$("remote-backups");container.hidden=false;container.replaceChildren();
  if(!result.backups.length) container.textContent="远程存储中还没有备份。";
  for(const backup of result.backups) {const row=document.createElement("div"),label=document.createElement("span");row.className="remote-row";label.textContent=`${formatDate(backup.created_at)} · ${size(backup.size)}`;row.append(label,button("取回并选择",()=>action("remote-download",{name:backup.name},file=>{selectBackup({...file,label:backup.name});message("远程副本已取回，可以验证备份。");})));container.append(row);}
})));
$("archive-upload").addEventListener("change",safeHandler(async()=>{
  const file=$("archive-upload").files[0];if(!file)return;if(file.size>2*1024**3)throw new Error("备份文件超过 2 GiB 限制");
  if(file.size===0)throw new Error("备份文件为空");invalidate();let id=null;
  for(let offset=0;offset<file.size;offset+=4*1024**2) {const end=Math.min(offset+4*1024**2,file.size);const query=new URLSearchParams({offset:String(offset),complete:String(end===file.size)});if(id)query.set("id",id);const result=await api("/api/recovery/upload?"+query,{method:"POST",headers:{"Content-Type":"application/octet-stream"},body:file.slice(offset,end)});id=result.id;message(`正在上传备份：${Math.round(end/file.size*100)}%`);if(end===file.size)selectBackup({...result,label:file.name});}
  message("备份上传完成，请选择恢复密钥并验证。");
}));
$("discard-upload").addEventListener("click",safeHandler(async()=>{if(selected?.imported)await action("discard-upload",{name:selected.name});selected=null;invalidate();$("selected-backup").textContent="尚未选择备份";$("discard-upload").hidden=true;$("archive-upload").value="";message("暂存文件已删除。");}));
$("restore-key").addEventListener("change",safeHandler(async()=>{restoreKey=await fileText($("restore-key"));invalidate();}));
$("inspect").addEventListener("click",safeHandler(async()=>{
  if(!selected)throw new Error("请先选择或上传备份");if(!restoreKey)throw new Error("请选择恢复密钥");
  const source=selected,key=restoreKey;
  await action("inspect",{name:source.name,imported:source.imported || false,key},result=>{
    if(selected!==source || restoreKey!==key)return;validated=true;$("preview").textContent=`${result.message} 备份时间：${formatDate(result.created_at)}；文件 ${result.files} 个，共 ${size(result.bytes)}。`;
    $("restore-confirmation").hidden=false;message("验证通过。确认后即可开始恢复。");
  });
}));
$("restore").addEventListener("click",safeHandler(async()=>{
  if(!validated || !selected)throw new Error("请先验证备份");if($("confirm-restore").value!=="恢复此站点")throw new Error("请输入“恢复此站点”确认覆盖");
  await action("restore",{name:selected.name,imported:selected.imported || false,key:restoreKey,confirm:$("confirm-restore").value,allow_without_snapshot:$("without-snapshot").checked,database_url:$("target-database").value,public_base_url:$("target-origin").value},()=>{invalidate();message("恢复已完成。请返回后台，使用备份中的管理员账号重新登录。");});
}));
$("target-origin").value=location.origin;
refresh().catch(()=>{});
setInterval(()=>{if(csrf)refresh().catch(error=>message(error.message,true));},3000);
