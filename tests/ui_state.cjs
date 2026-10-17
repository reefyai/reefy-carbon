// Browser-independent regression tests for collapsed panels and QR loading states.
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const elements = new Map();
const el = id => {
  if (!elements.has(id)) elements.set(id, {open:false,hidden:true,disabled:false,options:[],value:'',textContent:'',setAttribute(key,value){this[key]=value},focus(){document.activeElement=this},addEventListener(){},removeAttribute(key){delete this[key]},replaceChildren(...items){this.children=items},add(){}});
  return elements.get(id);
};
let current = {whatsapp:'Not linked',has_qr:false,qr_expires_in:0,model:null,true_admins:null};
function node() {return {children:[],open:false,textContent:'',listeners:{},append(...items){this.children.push(...items)},addEventListener(name,fn){this.listeners[name]=fn}}}
const navLinks=['channels','model','admin','activity'].map(page=>({dataset:{page},attributes:{},setAttribute(key,value){this.attributes[key]=value},removeAttribute(key){delete this.attributes[key]},addEventListener(){},focus(){document.activeElement=this}}));
const classes=new Set();
const mobileNavigationFixture={matches:false,addEventListener(){}};
const document = {body:{classList:{toggle(key,on){if(on)classes.add(key);else classes.delete(key)}}},createElement:node,hidden:false,getElementById:el,querySelectorAll:()=>navLinks,addEventListener(){}};
const context = vm.createContext({document,window:{location:{hash:''},history:{pushState(){}},matchMedia:()=>mobileNavigationFixture,addEventListener(){}},localStorage:{getItem(){return null},setItem(){}},crypto:{randomUUID:()=> 'test-viewer'},setInterval(){},Option:function(text,value){this.textContent=text;this.value=value},fetch:async()=>({ok:true,status:200,json:async()=>current}),Date,console});
vm.runInContext(fs.readFileSync('web/app.js','utf8'),context);
(async()=>{
  await vm.runInContext('status()',context);
  assert.equal(el('qr-loading').hidden,true);
  el('whatsapp-panel').open=true;
  await vm.runInContext('status()',context);
  assert.equal(el('qr-loading').hidden,false);
  current={...current,has_qr:true,qr_version:'one',qr_expires_in:45};
  await vm.runInContext('status()',context);
  assert.equal(el('qr-loading').hidden,true);
  assert.equal(el('qr').hidden,false);
  vm.runInContext('qrExpires=Date.now()-1;updateQrExpiry()',context);
  assert.equal(el('qr-loading').hidden,false);
  assert.equal(el('qr').hidden,true);
  vm.runInContext('codeExpires=Date.now()+60000;updateQrExpiry()',context);
  assert.equal(el('qr-loading').hidden,true);
  vm.runInContext('codeExpires=0',context);
  document.hidden=true;
  vm.runInContext('updateQrExpiry()',context);
  assert.equal(el('qr-loading').hidden,true);
  document.hidden=false;
  for (const state of ['Connected','Unlinking…']) {
    current={...current,whatsapp:state,has_qr:false};
    await vm.runInContext('status()',context);
    assert.equal(el('qr-loading').hidden,true);
  }
  current={...current,whatsapp:'Logged out',relink_required:true,has_qr:false};
  await vm.runInContext('status()',context);
  assert.equal(el('qr-loading').hidden,true,'terminal logout must not spin forever');
  assert.equal(el('unlink-whatsapp').hidden,false,'logged-out users need a relink action');
  assert.equal(el('unlink-whatsapp').textContent,'Relink WhatsApp');
  assert.equal(el('relink-help').hidden,false);
  assert.equal(el('request-code').disabled,true);
  current={...current,whatsapp:'Disconnected',relink_required:false};
  await vm.runInContext('status()',context);
  assert.equal(el('unlink-whatsapp').hidden,true,'a transient disconnect must not offer destructive reset');
  assert.equal(el('qr-loading').hidden,false);
  const html=fs.readFileSync('web/index.html','utf8');
  const panels=html.match(/<details[^>]*>/g);
  assert.equal(panels.length,1);
  assert.ok(panels.every(tag=>!tag.includes(' open')));
  assert.match(html,/id="qr-loading" role="status" hidden/);
  vm.runInContext('renderApprovals([{summary:"Create group <script>?",state:"pending",identity_label:"+15555550123 (test@lid)",expires:2000000000}])',context);
  assert.equal(el('approvals-list').children[0].children[0].textContent,'Create group <script>?');
  assert.match(el('approvals-list').children[0].children[1].textContent,/pending.*15555550123/);
  vm.runInContext('renderApprovals([])',context);
  assert.equal(el('approvals-status').textContent,'No approval requests.');
  const events=[
    {id:4,created:12,operation_id:'one',identity:'same@test',level:'info',event:'reply_sent',message:'Reply sent'},
    {id:3,created:11,operation_id:'two',identity:'same@test',level:'error',event:'llm_failed',message:'Failed'},
    {id:2,created:10,operation_id:'two',identity:'same@test',level:'info',event:'message_received',message:'Queued'},
    {id:1,created:10,operation_id:'one',identity:'same@test',level:'info',event:'message_received',message:'Queued'},
    {id:5,created:13,operation_id:null,level:'info',event:'connected',message:'Connected'}
  ];
  context.activityFixture=events;
  let groups=vm.runInContext('groupActivity(activityFixture)',context);
  assert.equal(groups.length,3);assert.equal(groups[1].key,'one');assert.equal(groups[1].events[0].id,1);assert.equal(groups[1].events[1].id,4);
  vm.runInContext('renderActivity(activityFixture)',context);
  let rows=el('activity-list').children;
  assert.ok(rows.every(row=>!row.children[0].open));
  assert.match(rows[1].children[0].children[0].textContent,/Reply sent.*2s/);
  assert.match(rows[2].children[0].children[0].textContent,/1 error/);
  rows[1].children[0].open=true;rows[1].children[0].listeners.toggle();
  vm.runInContext('renderActivity(activityFixture)',context);
  assert.equal(el('activity-list').children[1].children[0].open,true);
  assert.equal(el('activity-list').children[2].children[0].open,false);
  assert.equal(el('admin-rows').children.length,1);
  el('add-admin').onclick();el('add-admin').onclick();
  assert.equal(el('admin-rows').children.length,3);
  const third=el('admin-rows').children[2];third.children[2].listeners.click();
  assert.equal(el('admin-rows').children.length,2);
  vm.runInContext("participantOptions=[{id:'person',identity:'123@lid',label:'+15550000001 (123@lid)'}];renderAdmins()",context);
  assert.equal(el('admin-rows').children[0].children[1].children[1].textContent,'+15550000001 (123@lid)');
  current={...current,true_admins:['person']};await vm.runInContext('status()',context);
  assert.equal(el('admin-rows').children.length,1);assert.equal(el('admin-rows').children[0].children[1].disabled,true);
  assert.equal(el('add-admin').disabled,true);
  assert.match(el('reviewer-help').textContent,/Adding or removing an admin here takes effect immediately/);
  assert.equal(el('admin-rows').children[0].children.length,2);
  current={...current,true_admins:['person','new-admin']};await vm.runInContext('status()',context);
  assert.equal(el('admin-rows').children.length,2);
  assert.equal(el('admin-rows').children[1].children[1].children.at(-1).textContent,'Participant not loaded');
  assert.equal(el('admin-rows').children[0].children[2].textContent,'Remove admin');
  current={...current,true_admins:['new-admin']};await vm.runInContext('status()',context);
  assert.equal(el('admin-rows').children.length,1);
  assert.equal(el('admin-rows').children[0].children[1].disabled,true);
  assert.equal(el('admin-rows').children[0].children.length,2);
  current={...current,true_admins:['admin']};
  await vm.runInContext('status()',context);
  assert.equal(el('request-admin-form').hidden,false);
  assert.equal(el('save-admins').hidden,true);
  vm.runInContext("participantOptions=[{id:'admin',label:'Admin'},{id:'candidate',label:'Candidate'}];renderAdmins()",context);
  assert.equal(el('admin-candidate').children.length,2);
  assert.equal(el('admin-candidate').children[1].value,'candidate');
  vm.runInContext("selectPage('admin',false,false)",context);
  assert.equal(el('admin-page').hidden,false);
  assert.equal(el('channels-page').hidden,true);
  assert.equal(el('whatsapp-panel').open,false);
  assert.equal(navLinks[2].attributes['aria-current'],'page');
  assert.equal(navLinks[0].attributes['aria-current'],undefined);
  el('nav-toggle').onclick();assert.equal(classes.has('nav-collapsed'),true);
  assert.equal(el('nav-toggle')['aria-expanded'],'false');
  mobileNavigationFixture.matches=true;
  el('mobile-nav-toggle').onclick();assert.equal(classes.has('nav-mobile-open'),true);
  assert.equal(el('main-content').inert,true);
  el('nav-backdrop').onclick();assert.equal(classes.has('nav-mobile-open'),false);
  assert.equal(el('app-navigation').inert,true);
  assert.equal(el('main-content').inert,false);
  assert.match(html,/aria-label="Carbon navigation"/);
  assert.match(html,/id="admin-page"/);
  assert.ok(!html.includes('<summary>Admin approval'));
  console.log('UI state regression checks passed');
})().catch(e=>{console.error(e);process.exitCode=1});
