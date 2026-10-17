'use strict';
const $ = id => document.getElementById(id);
let activePage = 'channels';
function pairingVisible() { return activePage === 'channels' && $('whatsapp-panel').open && !document.hidden; }
let selectedModel = null;
let codeExpires = 0;
let pairingBusy = false;
let qrVersion = null;
let qrExpires = 0;
let connectionState = 'Loading…';
let relinkRequired = false;
async function api(path, body) {
  const response = await fetch(path, body === undefined ? {} : { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Carbon-Request': '1' }, body: JSON.stringify(body) });
  if (!response.ok) throw new Error((await response.text()).slice(0, 200) || `Request failed (${response.status})`);
  return response.status === 204 ? null : response.json();
}
function showError(error) { $('error').textContent = error.message; $('error').hidden = false; }
async function status() {
  try {
    const data = await api('/api/status');
    $('connection').textContent = data.whatsapp;
    connectionState = data.whatsapp;
    relinkRequired = data.relink_required === true;
    const showQr = pairingVisible() && data.has_qr;
    $('qr').hidden = !showQr;
    qrExpires = showQr ? Date.now() + data.qr_expires_in * 1000 : 0;
    if (showQr && qrVersion !== data.qr_version) {
      qrVersion = data.qr_version;
      $('qr').src = '/api/whatsapp/qr.svg?v=' + encodeURIComponent(qrVersion);
    }
    if (!showQr) { qrVersion = null; $('qr').removeAttribute('src'); }
    $('qr-expiry').hidden = data.whatsapp === 'Connected' || data.whatsapp === 'Unlinking…';
    updateQrExpiry();
    const connected = data.whatsapp === 'Connected';
    $('unlink-whatsapp').hidden = !connected && !relinkRequired;
    $('unlink-whatsapp').textContent = relinkRequired ? 'Relink WhatsApp' : 'Unlink WhatsApp';
    $('relink-help').hidden = !relinkRequired || data.whatsapp === 'Unlinking…';
    $('pairing-help').hidden = connected || relinkRequired || data.whatsapp === 'Unlinking…';
    if (connected) { codeExpires = 0; $('pair-code').textContent = ''; $('pair-code-result').hidden = true; }
    $('request-code').disabled = connected || relinkRequired || pairingBusy;
    selectedModel = data.model;
    if (data.true_admins) {
      const changed=JSON.stringify(sealedAdmins)!==JSON.stringify(data.true_admins);
      sealedAdmins=data.true_admins;
      for(const id of ['save-admins','add-admin']) $(id).disabled=true;
      $('reviewer-help').textContent='Current admins. Additions require approval through WhatsApp.';
      $('add-admin').hidden=true; $('save-admins').hidden=true;
      $('request-admin-form').hidden=false;
      $('admin-help').textContent='Any admin can use WhatsApp Reply on a request and send approve or reject, including for their own request.';
      if(changed) {renderAdmins(); if(activePage === 'admin') participants();}
    }
  } catch (error) { showError(error); }
}
async function models() {
  $('reload-models').disabled = true;
  try {
    const data = await api('/api/models');
    $('model').replaceChildren(...data.models.map(id => new Option(id, id)));
    $('model').disabled = !data.models.length;
    $('save-model').disabled = !data.models.length;
    if (selectedModel) $('model').value = selectedModel;
    $('model-help').textContent = data.models.length ? (selectedModel ? `Selected: ${selectedModel}` : 'Choose a model to enable replies.') : 'No models reported. Attach a provider above or configure one in Reefy.';
  } catch (_) { $('model-help').textContent = 'No models available. Attach a provider above, or configure the Reefy LLM proxy, then refresh.'; }
  finally { $('reload-models').disabled = false; }
}
const upcomingProviders = [{slug: 'codex', name: 'OpenAI Codex', upcoming: true}];
let providerData = null;
let providerBusy = false;
let providerTimer = null;
function providerState(p) {
  if (p.upcoming) return 'Coming soon';
  if (p.attached) return 'Attached';
  return p.pending ? 'Waiting for sign-in…' : 'Not attached';
}
function renderProviders() {
  const rows = [...(providerData || []), ...upcomingProviders.filter(u => !(providerData || []).some(p => p.slug === u.slug))];
  $('provider-rows').replaceChildren(...rows.map(p => {
    const row = document.createElement('div'); row.className = 'provider-row';
    const head = document.createElement('div'); head.className = 'provider-head';
    const name = document.createElement('strong'); name.textContent = p.name;
    const state = document.createElement('span'); state.className = 'status'; state.textContent = providerState(p);
    head.append(name, state); row.append(head);
    if (p.pending) {
      const link = document.createElement('a');
      link.href = p.pending.verification_uri_complete || p.pending.verification_uri;
      link.target = '_blank'; link.rel = 'noopener noreferrer'; link.textContent = p.pending.verification_uri;
      const open = document.createElement('p'); open.append('1. Open ', link);
      const enter = document.createElement('p'); enter.textContent = '2. Sign in and confirm this code:';
      const code = document.createElement('output'); code.className = 'device-code'; code.textContent = p.pending.user_code;
      const expiry = document.createElement('p'); expiry.className = 'provider-expiry';
      expiry.dataset.expires = String(Date.now() + p.pending.expires_in * 1000);
      row.append(open, enter, code, expiry);
    }
    if (p.error) { const error = document.createElement('p'); error.className = 'provider-error'; error.textContent = p.error; row.append(error); }
    const actions = document.createElement('div'); actions.className = 'actions';
    const button = document.createElement('button'); button.type = 'button';
    if (p.upcoming) { button.textContent = 'Attach'; button.disabled = true; }
    else if (p.attached) { button.textContent = 'Detach'; button.onclick = () => providerAction(p, 'detach'); }
    else if (p.pending) { button.textContent = 'Cancel'; button.onclick = () => providerAction(p, 'detach'); }
    else { button.textContent = 'Attach'; button.className = 'provider-attach'; button.onclick = () => providerAction(p, 'attach'); }
    actions.append(button); row.append(actions);
    return row;
  }));
  updateProviderExpiry();
}
function updateProviderExpiry() {
  for (const element of document.querySelectorAll('.provider-expiry')) {
    const seconds = Math.max(0, Math.ceil((Number(element.dataset.expires) - Date.now()) / 1000));
    element.textContent = seconds ? `Code expires in ${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}. This page updates when you finish.` : 'Code expired. Click Cancel, then Attach again.';
  }
}
setInterval(updateProviderExpiry, 1000);
async function providers() {
  if (providerBusy) return;
  providerBusy = true;
  clearTimeout(providerTimer);
  try {
    const before = providerData && new Set(providerData.filter(p => p.attached).map(p => p.slug));
    providerData = (await api('/api/llm-providers')).providers;
    renderProviders();
    if (before && providerData.some(p => p.attached && !before.has(p.slug))) models();
  } catch (error) { showError(error); }
  finally { providerBusy = false; }
  if (activePage === 'model' && providerData?.some(p => p.pending)) providerTimer = setTimeout(providers, 2000);
}
async function providerAction(p, action) {
  if (action === 'detach' && p.attached && !confirm(`Detach ${p.name}? Carbon will stop using this subscription.`)) return;
  $('error').hidden = true;
  for (const button of $('provider-rows').querySelectorAll('button')) button.disabled = true;
  try {
    providerData = (await api(`/api/llm-providers/${encodeURIComponent(p.slug)}/${action}`, {})).providers;
    if (action === 'detach') models();
  } catch (error) { showError(error); }
  renderProviders();
  providers();
}
let participantOptions=[];
let selectedAdmins=[''];
let sealedAdmins=null;
function renderAdmins() {
  const ids=sealedAdmins || selectedAdmins;
  $('admin-rows').replaceChildren(...ids.map((value,index)=>{
    const row=document.createElement('div');row.className='admin-row';
    const label=document.createElement('label');label.textContent=`Admin ${index+1}`;label.htmlFor=`admin-select-${index}`;
    const select=document.createElement('select');select.id=`admin-select-${index}`;select.disabled=!!sealedAdmins;
    select.append(new Option('Choose participant',''),...participantOptions.map(p=>new Option(p.label || p.identity,p.id)));
    if(value && !participantOptions.some(p=>p.id===value)) select.append(new Option('Participant not loaded',value));
    select.value=value;select.addEventListener('change',()=>{selectedAdmins[index]=select.value});
    row.append(label,select);
    if(!sealedAdmins && ids.length>1) {const remove=document.createElement('button');remove.type='button';remove.textContent='Remove';remove.addEventListener('click',()=>{selectedAdmins.splice(index,1);renderAdmins()});row.append(remove);}
    return row;
  }));
  const candidate=$('admin-candidate');
  const previous=candidate.value;
  candidate.replaceChildren(new Option('Choose participant',''), ...participantOptions.filter(p=>!(sealedAdmins || []).includes(p.id)).map(p=>new Option(p.label || p.identity,p.id)));
  candidate.value=previous;
}
$('request-admin-form').onsubmit=async event=>{
  event.preventDefault();
  if(!$('admin-candidate').value) {showError(new Error('Choose a participant'));return;}
  $('request-admin').disabled=true;
  try {
    await api('/api/admins/requests',{user:$('admin-candidate').value});
    $('admin-request-status').textContent='Pending approval. An existing admin must reply approve to the WhatsApp request.';
  } catch(error) {showError(error);}
  finally {$('request-admin').disabled=false;}
};
async function participants() {
  try {participantOptions=await api('/api/participants');renderAdmins();}
  catch(error){showError(error)}
}
$('add-admin').onclick=()=>{if(!sealedAdmins && selectedAdmins.length<1000){selectedAdmins.push('');renderAdmins();}};
renderAdmins();
$('reload-models').onclick = models;
$('reload-participants').onclick = participants;
$('save-model').onclick = async () => { try { await api('/api/model', { model: $('model').value }); selectedModel = $('model').value; $('model-help').textContent = `Selected: ${selectedModel}`; } catch (error) { showError(error); } };
$('save-admins').onclick = async () => { try { await api('/api/admins/bootstrap', selectedAdmins.filter(Boolean)); await status(); } catch (error) { showError(error); } };
(async () => { await status(); setInterval(status, 5000); })();

$('phone-link').onsubmit = async event => {
  event.preventDefault();
  if (pairingBusy) return;
  pairingBusy = true;
  $('request-code').disabled = true;
  $('error').hidden = true;
  try {
    const data = await api('/api/whatsapp/pair-code', { phone_number: $('phone-number').value });
    $('pair-code').textContent = data.code;
    codeExpires = Date.now() + data.expires_in * 1000;
    $('pair-code-result').hidden = false;
    updateCodeExpiry();
  } catch (error) { showError(error); }
  finally { pairingBusy = false; $('request-code').disabled = false; }
};
function updateCodeExpiry() {
  if (!codeExpires) return;
  const seconds = Math.max(0, Math.ceil((codeExpires - Date.now()) / 1000));
  $('pair-code-expiry').textContent = seconds ? `Expires in ${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}` : 'Code expired. Request a new code.';
  if (!seconds) { $('pair-code').textContent = ''; codeExpires = 0; }
}
setInterval(updateCodeExpiry, 1000);

$('unlink-whatsapp').onclick = () => {
  $('whatsapp-confirm-title').textContent = relinkRequired ? 'Relink WhatsApp?' : 'Unlink WhatsApp?';
  $('whatsapp-confirm-submit').textContent = relinkRequired ? 'Relink WhatsApp' : 'Unlink WhatsApp';
  $('whatsapp-confirm-description').textContent = relinkRequired
    ? 'Replace the expired WhatsApp link and cancel queued replies? Saved conversations and files will remain. You will need to link your phone again.'
    : 'Unlink WhatsApp? Carbon will stop replying and cancel queued replies. Saved conversations and files will remain.';
  $('whatsapp-confirm').showModal();
};
$('whatsapp-confirm-form').onsubmit = async event => {
  if (event.submitter?.value !== 'confirm') return;
  event.preventDefault();
  $('whatsapp-confirm').close();
  $('unlink-whatsapp').disabled = true;
  try {
    await api('/api/whatsapp/unlink', {});
    await status();
  } catch (error) { showError(error); }
  finally { $('unlink-whatsapp').disabled = false; }
};

function updateQrExpiry() {
  const seconds = Math.max(0, Math.ceil((qrExpires - Date.now()) / 1000));
  $('qr-expiry').textContent = seconds ? `QR refreshes automatically in ${seconds}s` : (codeExpires > Date.now() ? 'Enter your phone linking code. QR renewal resumes after it expires.' : 'Waiting for a fresh QR code…');
  if (!seconds) $('qr').hidden = true;
  $('qr-expiry').hidden = ['Connected', 'Unlinking…'].includes(connectionState) || (!seconds && codeExpires <= Date.now());
  $('qr-loading').hidden = !(pairingVisible() && !relinkRequired && !['Connected', 'Unlinking…'].includes(connectionState) && !seconds && codeExpires <= Date.now());
}
$('qr').onerror = () => { qrVersion = null; qrExpires = 0; $('qr').hidden = true; updateQrExpiry(); };
setInterval(updateQrExpiry, 1000);

let activityBusy = false;
const expandedOperations = new Set();
function groupActivity(events) {
  const groups = new Map();
  for (const event of events) {
    const key = event.operation_id || `system-${event.id}`;
    if (!groups.has(key)) groups.set(key, {key, operation: event.operation_id, events: []});
    groups.get(key).events.push(event);
  }
  return [...groups.values()].map(group => {
    group.events.sort((a,b) => a.id-b.id);
    group.first=group.events[0]; group.last=group.events[group.events.length-1];
    return group;
  }).sort((a,b) => b.last.id-a.last.id);
}
function renderActivity(events) {
  const groups=groupActivity(events);
  const present=new Set(groups.map(g=>g.key));
  for (const key of expandedOperations) if (!present.has(key)) expandedOperations.delete(key);
  $('activity-list').replaceChildren(...groups.map(group => {
    const row=document.createElement('li');
    const details=document.createElement('details');
    const summary=document.createElement('summary');
    const first=group.first, last=group.last;
    const errors=group.events.filter(e=>e.level==='error').length;
    const participant=group.events.find(e=>e.identity);
    const identity=participant?.identity_label || participant?.identity;
    const completeStart=['message_received','attachment_queued'].includes(first.event);
    const elapsed=Math.max(0,last.created-first.created);
    summary.textContent = `${new Date(first.created*1000).toLocaleString()} · ${group.operation?'Message':identity?'Event':'System'}${identity?` · ${identity}`:''} · ${last.message}${group.operation?` · ${completeStart?'':'≥'}${elapsed}s`:''}${errors?` · ${errors} error${errors===1?'':'s'}`:''}`;
    details.open=expandedOperations.has(group.key);
    details.addEventListener('toggle',()=>{
      if(details.open) expandedOperations.add(group.key); else expandedOperations.delete(group.key);
    });
    details.append(summary);
    if(group.operation) {
      const id=document.createElement('div'); id.className='operation-id'; id.textContent=`Operation ${group.operation}`; details.append(id);
      const trace=document.createElement('a'); trace.href=`/api/traces/${encodeURIComponent(group.operation)}`; trace.textContent='Download model trace'; details.append(trace);
    }
    const list=document.createElement('ol');
    for(const event of group.events) {
      const item=document.createElement('li');
      const title=document.createElement('div');
      title.textContent=`${new Date(event.created*1000).toLocaleTimeString()} · ${event.level.toUpperCase()} · ${event.event}`;
      const detail=document.createElement('div'); detail.textContent=event.message;
      item.append(title,detail); list.append(item);
    }
    details.append(list); row.append(details); return row;
  }));
  $('activity-status').textContent=events.length?'Recent events retained for up to 7 days / 1,000 events. Updates while open.':'No events recorded yet.';
}
async function activity() {
  if (activePage !== 'activity' || document.hidden || activityBusy) return;
  activityBusy = true;
  try { renderActivity((await api('/api/activity')).events); }
  catch (_) { $('activity-status').textContent = 'Cannot load activity. Try refreshing.'; }
  finally { activityBusy = false; }
}

$('reload-activity').onclick = activity;
setInterval(activity, 5000);

const pairingViewer = crypto.randomUUID();
let pairingViewQueue = Promise.resolve();
function updatePairingView() {
  const open = pairingVisible();
  if (!open) { qrExpires = 0; qrVersion = null; $('qr').hidden = true; $('qr').removeAttribute('src'); }
  updateQrExpiry();
  pairingViewQueue = pairingViewQueue.catch(() => {}).then(() => api('/api/whatsapp/pairing-view', { viewer: pairingViewer, open })).catch(showError);
}
$('whatsapp-panel').addEventListener('toggle', () => { updatePairingView(); if ($('whatsapp-panel').open) status(); });


document.addEventListener('visibilitychange', updatePairingView);
window.addEventListener('pagehide', () => {
  fetch('/api/whatsapp/pairing-view', { method: 'POST', keepalive: true, headers: { 'Content-Type': 'application/json', 'X-Carbon-Request': '1' }, body: JSON.stringify({ viewer: pairingViewer, open: false }) }).catch(() => {});
});
setInterval(() => { if (pairingVisible()) updatePairingView(); }, 10000);

let approvalsBusy=false;
function renderApprovals(requests) {
  $('approvals-list').replaceChildren(...requests.map(request=>{
    const item=document.createElement('li');
    const title=document.createElement('div'); title.textContent=request.summary;
    const state=document.createElement('div'); state.textContent=`${request.state} · ${request.identity_label || request.identity || 'Unknown participant'}`;
    const expiry=document.createElement('div'); expiry.textContent=`${request.state==='pending'?'Expires':'Expiry'}: ${new Date(request.expires*1000).toLocaleString()}`;
    item.append(title,state,expiry); return item;
  }));
  $('approvals-status').textContent=requests.length?'Latest 100 requests. Updates while open.':'No approval requests.';
}
async function approvals() {
  if (activePage !== 'admin' || approvalsBusy || document.hidden) return;
  approvalsBusy=true;
  try {renderApprovals((await api('/api/approval-requests')).requests);}
  catch (_) {$('approvals-status').textContent='Cannot load approval requests. Try refreshing.';}
  finally {approvalsBusy=false;}
}

$('reload-approvals').onclick=approvals;
setInterval(approvals,5000);

const pageNames = ['channels', 'model', 'admin', 'activity'];
const navLinks = Array.from(document.querySelectorAll('[data-page]'));
const mobileNavigation = window.matchMedia('(max-width: 720px)');
let navExpanded = true;
try { navExpanded = localStorage.getItem('carbon.navigation.expanded') !== 'false'; } catch (_) {}
let mobileNavOpen = false;
function renderNavigation() {
  document.body.classList.toggle('nav-collapsed', !navExpanded);
  document.body.classList.toggle('nav-mobile-open', mobileNavigation.matches && mobileNavOpen);
  $('nav-toggle').setAttribute('aria-expanded', String(navExpanded));
  const label = navExpanded ? 'Collapse navigation' : 'Expand navigation';
  $('nav-toggle').setAttribute('aria-label', label);
  $('nav-toggle').title = label;
  $('mobile-nav-toggle').setAttribute('aria-expanded', String(mobileNavOpen));
  const mobileLabel = mobileNavOpen ? 'Close navigation' : 'Open navigation';
  $('mobile-nav-toggle').setAttribute('aria-label', mobileLabel);
  $('mobile-nav-toggle').title = mobileLabel;
  $('nav-backdrop').hidden = !mobileNavigation.matches || !mobileNavOpen;
  $('app-navigation').inert = mobileNavigation.matches && !mobileNavOpen;
  $('main-content').inert = mobileNavigation.matches && mobileNavOpen;
}
function closeMobileNavigation(returnFocus = false) {
  mobileNavOpen = false;
  renderNavigation();
  if (returnFocus) $('mobile-nav-toggle').focus();
}
function toggleNavigation() {
  if (mobileNavigation.matches) mobileNavOpen = !mobileNavOpen;
  else {
    navExpanded = !navExpanded;
    try { localStorage.setItem('carbon.navigation.expanded', String(navExpanded)); } catch (_) {}
  }
  renderNavigation();
  if (mobileNavigation.matches && mobileNavOpen) $('nav-close').focus();
}
$('nav-toggle').onclick = toggleNavigation;
$('mobile-nav-toggle').onclick = toggleNavigation;
$('nav-close').onclick = () => closeMobileNavigation(true);
$('nav-backdrop').onclick = () => closeMobileNavigation(true);
mobileNavigation.addEventListener('change', () => closeMobileNavigation());
document.addEventListener('keydown', event => {
  if (!mobileNavigation.matches || !mobileNavOpen) return;
  if (event.key === 'Escape') { event.preventDefault(); closeMobileNavigation(true); }
  if (event.key === 'Tab') {
    const stops = [$('mobile-nav-toggle'), $('nav-close'), ...navLinks];
    const index = stops.indexOf(document.activeElement);
    if (event.shiftKey && index <= 0) { event.preventDefault(); stops.at(-1).focus(); }
    else if (!event.shiftKey && (index === stops.length - 1 || index < 0)) { event.preventDefault(); stops[0].focus(); }
  }
});
function selectPage(name, updateHash = true, focus = true) {
  activePage = pageNames.includes(name) ? name : 'channels';
  for (const page of pageNames) $(page + '-page').hidden = page !== activePage;
  for (const link of navLinks) {
    if (link.dataset.page === activePage) link.setAttribute('aria-current', 'page');
    else link.removeAttribute('aria-current');
  }
  if (activePage !== 'channels') $('whatsapp-panel').open = false;
  closeMobileNavigation();
  if (updateHash) window.history.pushState(null, '', '#' + activePage);
  if (focus) $(activePage + '-title').focus({preventScroll:true});
  updatePairingView();
  if (activePage === 'channels') status();
  if (activePage === 'model') { providers(); models(); }
  if (activePage === 'admin') { participants(); approvals(); }
  if (activePage === 'activity') activity();
}
for (const link of navLinks) link.addEventListener('click', event => { event.preventDefault(); selectPage(link.dataset.page); });
window.addEventListener('hashchange', () => selectPage(window.location.hash.slice(1), false));
renderNavigation();
selectPage(window.location.hash.slice(1), false, false);

$('skip-content').addEventListener('click', event => { event.preventDefault(); $('main-content').focus({preventScroll:true}); window.scrollTo(0, 0); });
