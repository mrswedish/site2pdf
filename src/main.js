const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ── Elements ──────────────────────────────────────────────────────────────────
const phaseSetup    = document.getElementById('phase-setup');
const phaseInput    = document.getElementById('phase-input');
const phasePrepare  = document.getElementById('phase-prepare');
const phaseProgress = document.getElementById('phase-progress');
const phaseDone     = document.getElementById('phase-done');

const setupBar      = document.getElementById('setup-bar');
const setupLabel    = document.getElementById('setup-label');
const setupStartBtn = document.getElementById('setup-start-btn');

const urlInput          = document.getElementById('url');
const outputInput       = document.getElementById('output-path');
const maxDepthInput     = document.getElementById('max-depth');
const browseBtn         = document.getElementById('browse-btn');
const startBtn          = document.getElementById('start-btn');
const inputError        = document.getElementById('input-error');
const manualCookiesCb   = document.getElementById('manual-cookies');
const blockedInput      = document.getElementById('blocked-patterns');

const modeCrawlBtn  = document.getElementById('mode-crawl');
const modeListBtn   = document.getElementById('mode-list');
const crawlFields   = document.getElementById('crawl-fields');
const listFields    = document.getElementById('list-fields');
const urlListInput  = document.getElementById('url-list');
const loadUrlsBtn   = document.getElementById('load-urls-btn');

const prepareStartBtn   = document.getElementById('prepare-start-btn');
const prepareCancelBtn  = document.getElementById('prepare-cancel-btn');

const progressLabel = document.getElementById('progress-label');
const progressBar   = document.getElementById('progress-bar');
const progressUrl   = document.getElementById('progress-url');
const cancelBtn     = document.getElementById('cancel-btn');

const doneMessage   = document.getElementById('done-message');
const openBtn       = document.getElementById('open-btn');
const restartBtn    = document.getElementById('restart-btn');

let lastOutputPath = '';
let unlisten = [];
let pendingCrawlParams = null;
let currentMode = 'crawl'; // 'crawl' | 'list'

// ── Mode toggle ───────────────────────────────────────────────────────────────
function setMode(mode) {
  currentMode = mode;
  const isCrawl = mode === 'crawl';
  modeCrawlBtn.classList.toggle('active', isCrawl);
  modeListBtn.classList.toggle('active', !isCrawl);
  crawlFields.classList.toggle('hidden', !isCrawl);
  listFields.classList.toggle('hidden', isCrawl);
  updateStartBtn();
}

modeCrawlBtn.addEventListener('click', () => setMode('crawl'));
modeListBtn.addEventListener('click', () => setMode('list'));

loadUrlsBtn.addEventListener('click', async () => {
  const content = await invoke('read_url_file');
  if (content != null) {
    urlListInput.value = content.trim();
    updateStartBtn();
  }
});

urlListInput.addEventListener('input', updateStartBtn);

// ── Boot: check if Chromium is already present ────────────────────────────────
async function boot() {
  const ready = await invoke('chromium_ready');
  showPhase(ready ? 'input' : 'setup');
}

// ── Setup phase (first-run Chromium download) ─────────────────────────────────
setupStartBtn.addEventListener('click', async () => {
  setupStartBtn.disabled = true;
  setupLabel.textContent = 'Ansluter…';

  const unlistenProgress = await listen('chromium-download-progress', ({ payload: p }) => {
    setupBar.style.width = p.percent + '%';
    setupLabel.textContent = `${p.downloadedMb.toFixed(0)} / ${p.totalMb.toFixed(0)} MB`;
  });

  try {
    await invoke('download_chromium');
    unlistenProgress();
    showPhase('input');
  } catch (err) {
    unlistenProgress();
    setupLabel.textContent = 'Nedladdning misslyckades: ' + err;
    setupStartBtn.disabled = false;
  }
});

// ── Input phase ───────────────────────────────────────────────────────────────
function updateStartBtn() {
  const hasOutput = !!outputInput.value.trim();
  const inputReady = currentMode === 'crawl'
    ? urlInput.value.trim().startsWith('http')
    : urlListInput.value.trim().split('\n').some(l => l.trim().startsWith('http'));
  startBtn.disabled = !(inputReady && hasOutput);
  inputError.classList.add('hidden');
}

urlInput.addEventListener('input', updateStartBtn);

browseBtn.addEventListener('click', async () => {
  const path = await invoke('choose_save_path');
  if (path) {
    outputInput.value = path;
    lastOutputPath = path;
    updateStartBtn();
  }
});

startBtn.addEventListener('click', async () => {
  const outputPath = outputInput.value.trim();
  if (!outputPath) return;

  const blockedPatterns = blockedInput.value
    .split('\n')
    .map(s => s.trim())
    .filter(s => s.length > 0);

  if (currentMode === 'list') {
    const urlList = urlListInput.value
      .split('\n')
      .map(s => s.trim())
      .filter(s => s.startsWith('http'));
    if (urlList.length === 0) return;
    beginCrawl({ url: '', outputPath, maxDepth: null, blockedPatterns, urlList });
    return;
  }

  const url = urlInput.value.trim();
  if (!url) return;

  const maxDepthVal = maxDepthInput.value.trim();
  const maxDepth = maxDepthVal === '' ? null : parseInt(maxDepthVal, 10);

  if (manualCookiesCb.checked) {
    // Open a visible browser so the user can handle cookie banners manually
    pendingCrawlParams = { url, outputPath, maxDepth, blockedPatterns, urlList: null };
    try {
      await invoke('open_preview_browser', { url });
      showPhase('prepare');
    } catch (err) {
      showError(String(err));
    }
    return;
  }

  // Direct crawl (no manual cookie handling)
  beginCrawl({ url, outputPath, maxDepth, blockedPatterns, urlList: null });
});

// ── Prepare phase ─────────────────────────────────────────────────────────────
prepareStartBtn.addEventListener('click', () => {
  beginCrawl({ ...pendingCrawlParams });
  pendingCrawlParams = null;
});

prepareCancelBtn.addEventListener('click', async () => {
  await invoke('close_preview_browser').catch(() => {});
  pendingCrawlParams = null;
  showPhase('input');
});

// ── Start the actual crawl ────────────────────────────────────────────────────
async function beginCrawl({ url, outputPath, maxDepth, blockedPatterns, urlList = null }) {
  showPhase('progress');
  progressBar.style.width = '0%';
  progressLabel.textContent = urlList ? 'Bearbetar URL-lista…' : 'Startar krypning…';
  progressUrl.textContent = '';

  unlisten.push(await listen('crawl-progress', ({ payload: p }) => {
    const pct = p.found > 0 ? Math.min(95, (p.done / p.found) * 100) : 0;
    progressBar.style.width = pct + '%';
    progressLabel.textContent = `${p.done} / ${p.found} sidor`;
    progressUrl.textContent = p.currentUrl;
  }));

  unlisten.push(await listen('crawl-complete', ({ payload: info }) => {
    cleanup();
    showPhase('done');
    const mb = (info.fileSize / 1024 / 1024).toFixed(1);
    doneMessage.textContent = `✓ PDF sparad\n${info.total} sidor · ${mb} MB\n${info.outputPath}`;
    lastOutputPath = info.outputPath;
  }));

  unlisten.push(await listen('crawl-error', ({ payload: msg }) => {
    cleanup();
    showPhase('input');
    showError(msg);
  }));

  try {
    await invoke('start_crawl', { url, outputPath, maxDepth, blockedPatterns, urlList });
  } catch (err) {
    cleanup();
    showPhase('input');
    showError(String(err));
  }
}

cancelBtn.addEventListener('click', async () => {
  await invoke('cancel_crawl').catch(() => {});
  cleanup();
  showPhase('input');
});

openBtn.addEventListener('click', async () => {
  if (lastOutputPath) await invoke('open_file', { path: lastOutputPath }).catch(() => {});
});

restartBtn.addEventListener('click', () => showPhase('input'));

// ── Helpers ───────────────────────────────────────────────────────────────────
function showPhase(name) {
  phaseSetup.classList.toggle('hidden', name !== 'setup');
  phaseInput.classList.toggle('hidden', name !== 'input');
  phasePrepare.classList.toggle('hidden', name !== 'prepare');
  phaseProgress.classList.toggle('hidden', name !== 'progress');
  phaseDone.classList.toggle('hidden', name !== 'done');
}

function showError(msg) {
  inputError.textContent = msg;
  inputError.classList.remove('hidden');
}

function cleanup() {
  unlisten.forEach(fn => fn());
  unlisten = [];
}

boot();
