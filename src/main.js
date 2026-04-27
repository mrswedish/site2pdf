const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Elements
const phaseInput    = document.getElementById('phase-input');
const phaseProgress = document.getElementById('phase-progress');
const phaseDone     = document.getElementById('phase-done');

const urlInput      = document.getElementById('url');
const outputInput   = document.getElementById('output-path');
const maxDepthInput = document.getElementById('max-depth');
const browseBtn     = document.getElementById('browse-btn');
const startBtn      = document.getElementById('start-btn');
const inputError    = document.getElementById('input-error');

const progressLabel = document.getElementById('progress-label');
const progressBar   = document.getElementById('progress-bar');
const progressUrl   = document.getElementById('progress-url');
const cancelBtn     = document.getElementById('cancel-btn');

const doneMessage   = document.getElementById('done-message');
const openBtn       = document.getElementById('open-btn');
const restartBtn    = document.getElementById('restart-btn');

let lastOutputPath  = '';
let unlisten        = [];

// Enable start button only when both fields are filled
function updateStartBtn() {
  const valid = urlInput.value.trim().startsWith('http') && outputInput.value.trim() !== '';
  startBtn.disabled = !valid;
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
  const url = urlInput.value.trim();
  if (!url) return;

  const outputPath = outputInput.value.trim();
  if (!outputPath) return;

  const maxDepthVal = maxDepthInput.value.trim();
  const maxDepth = maxDepthVal === '' ? null : parseInt(maxDepthVal, 10);

  showPhase('progress');
  progressBar.style.width = '0%';
  progressLabel.textContent = 'Startar krypning…';
  progressUrl.textContent = '';

  // Subscribe to progress events
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
    doneMessage.textContent =
      `✓ PDF sparad\n${info.total} sidor · ${mb} MB\n${info.outputPath}`;
    lastOutputPath = info.outputPath;
  }));

  unlisten.push(await listen('crawl-error', ({ payload: msg }) => {
    cleanup();
    showPhase('input');
    showError(msg);
  }));

  try {
    await invoke('start_crawl', { url, outputPath, maxDepth });
  } catch (err) {
    cleanup();
    showPhase('input');
    showError(String(err));
  }
});

cancelBtn.addEventListener('click', async () => {
  await invoke('cancel_crawl').catch(() => {});
  cleanup();
  showPhase('input');
});

openBtn.addEventListener('click', async () => {
  if (lastOutputPath) {
    await invoke('open_file', { path: lastOutputPath }).catch(() => {});
  }
});

restartBtn.addEventListener('click', () => {
  showPhase('input');
});

function showPhase(name) {
  phaseInput.classList.toggle('hidden', name !== 'input');
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
