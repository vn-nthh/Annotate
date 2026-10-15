const { invoke } = window.__TAURI__.core;
const { listen, emit } = window.__TAURI__.event;
const { load } = window.__TAURI__.store;
const { getCurrentWindow } = window.__TAURI__.window;

import * as sync from './gdrive-sync.js';
import {
  normalizeDictionaryStore,
  normalizeHistoryStore,
  normalizeKey,
} from './sync-merge.mjs';

// ── State ──────────────────────────────────────────────
let store = null;
let isRecording = false;
let isProcessing = false; // true while async transcribe is in-flight
let mediaRecorder = null;
let audioChunks = [];
let speechRecognition = null;

// ── DOM Elements ───────────────────────────────────────
const micSelect = document.getElementById('mic-select');
const modeSelect = document.getElementById('mode-select');
const apikeySection = document.getElementById('section-apikey');
const apikeyLabel = document.getElementById('apikey-label');
const apikeyInput = document.getElementById('apikey-input');
const apikeyToggle = document.getElementById('apikey-toggle');
const azureConfigSection = document.getElementById('section-azure-config');
const azureEndpointInput = document.getElementById('azure-endpoint-input');
const openrouterKeyInput = document.getElementById('openrouter-key-input');
const openrouterKeyToggle = document.getElementById('openrouter-key-toggle');
const hotkeyBtn = document.getElementById('hotkey-btn');
const hotkeyDisplay = document.getElementById('hotkey-display');
const dictHotkeyBtn = document.getElementById('dict-hotkey-btn');
const dictHotkeyDisplay = document.getElementById('dict-hotkey-display');
const dictHotkeyClear = document.getElementById('dict-hotkey-clear');
const statusText = document.getElementById('status-text');
const statusDot = document.getElementById('status-dot');
const historyList = document.getElementById('history-list');
const historyCount = document.getElementById('history-count');
const historyClear = document.getElementById('history-clear');
const dictInput = document.getElementById('dict-input');
const dictAddBtn = document.getElementById('dict-add-btn');
const dictList = document.getElementById('dict-list');
const dictCount = document.getElementById('dict-count');
const themeToggle = document.getElementById('theme-toggle');

// Local whisper elements
const sectionLocalWhisper = document.getElementById('section-local-whisper');
const accelerationSelect = document.getElementById('acceleration-select');
const whisperStatusText = document.getElementById('whisper-status-text');
const whisperDownloadBtn = document.getElementById('whisper-download-btn');
const whisperLoadBtn = document.getElementById('whisper-load-btn');
const whisperProgressWrap = document.getElementById('whisper-progress-wrap');
const whisperProgressFill = document.getElementById('whisper-progress-fill');
const whisperProgressText = document.getElementById('whisper-progress-text');

// CUDA / Vulkan runtime elements
const sectionCudaRuntime = document.getElementById('section-cuda-runtime');
const sectionVulkanRuntime = document.getElementById('section-vulkan-runtime');
const cudaStatusText = document.getElementById('cuda-status-text');
const cudaDownloadBtn = document.getElementById('cuda-download-btn');
const cudaProgressWrap = document.getElementById('cuda-progress-wrap');
const cudaProgressFill = document.getElementById('cuda-progress-fill');
const cudaProgressText = document.getElementById('cuda-progress-text');
const vulkanStatusText = document.getElementById('vulkan-status-text');

// GEC (grammar correction) elements
const grammarToggle = document.getElementById('grammar-toggle');
const gecModelSection = document.getElementById('gec-model-section');
const gecStatusText = document.getElementById('gec-status-text');
const gecDownloadBtn = document.getElementById('gec-download-btn');
const gecProgressWrap = document.getElementById('gec-progress-wrap');
const gecProgressFill = document.getElementById('gec-progress-fill');
const gecProgressText = document.getElementById('gec-progress-text');
let gecModelLoaded = false;

// Tab navigation
const tabBtns = document.querySelectorAll('.tab-btn');
const tabPanels = document.querySelectorAll('.tab-panel');

// ── Initialize ─────────────────────────────────────────
async function init() {
  store = await load('settings.json', { autoSave: true });

  await loadTheme();
  setupTabNavigation();
  setupEventListeners();
  await setupHotkeyListener();
  renderHistory();
  renderDictionary();

  // Defer slower feature initialization until after the first paint.
  void bootstrapDeferredStartup();
}

async function bootstrapDeferredStartup() {
  await afterFirstPaint();

  // Restore basic preferences first, then let feature panels finish in parallel.
  await Promise.allSettled([
    loadAudioDevices(),
    loadSavedSettings(),
  ]);

  await Promise.allSettled([
    initSyncUI(),
    initGrammarUI(),
    initSubtitlingUI(),
  ]);

  // Warm the local Whisper worker only after the UI is interactive. Vulkan device
  // init + model load is GPU-heavy and used to freeze startup when awaited here.
  scheduleWhisperWarmup(1800);
}

function afterFirstPaint() {
  return new Promise(resolve => {
    requestAnimationFrame(() => requestAnimationFrame(resolve));
  });
}

// ── Tab Navigation ─────────────────────────────────────
function setupTabNavigation() {
  tabBtns.forEach(btn => {
    btn.addEventListener('click', () => {
      const targetTab = btn.dataset.tab;

      // Update buttons
      tabBtns.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');

      // Update panels
      tabPanels.forEach(p => p.classList.remove('active'));
      document.getElementById(`tab-${targetTab}`).classList.add('active');
    });
  });
}

// ── Audio Devices ──────────────────────────────────────
async function loadAudioDevices() {
  try {
    const devices = await invoke('get_audio_devices');
    micSelect.innerHTML = '';

    if (devices.length === 0) {
      micSelect.innerHTML = '<option value="">No devices found</option>';
      return;
    }

    devices.forEach(device => {
      const opt = document.createElement('option');
      opt.value = device.id;
      opt.textContent = device.name;
      micSelect.appendChild(opt);
    });
  } catch (err) {
    console.error('Failed to load devices:', err);
    micSelect.innerHTML = '<option value="">Error loading devices</option>';
  }
}

// ── Settings Persistence ───────────────────────────────
async function loadSavedSettings() {
  const savedDevice = await store.get('audioDeviceId');
  if (savedDevice) {
    micSelect.value = savedDevice;
  }

  await restoreAccelerationPreference();

  const savedMode = await store.get('transcriptionMode');
  if (savedMode) {
    modeSelect.value = savedMode;
  }
  await toggleApiKeyVisibility(modeSelect.value);
  await loadOpenRouterKey();
  updateSubSummarizeBtn();

  const savedHotkey = await store.get('hotkey');
  if (savedHotkey) {
    hotkeyDisplay.textContent = savedHotkey;
    hotkeyDisplay.classList.add('hotkey-combo');

    // Retry registration
    const maxRetries = 3;
    const retryDelayMs = 500;
    let registered = false;

    for (let attempt = 1; attempt <= maxRetries; attempt++) {
      try {
        await invoke('register_hotkey', { shortcutStr: savedHotkey });
        setStatus('Ready', false);
        registered = true;
        break;
      } catch (err) {
        console.warn(`Hotkey registration attempt ${attempt}/${maxRetries} failed:`, err);
        if (attempt < maxRetries) {
          await new Promise(r => setTimeout(r, retryDelayMs));
        }
      }
    }

    if (!registered) {
      console.error('Hotkey registration failed after all retries');
      setStatus('Hotkey failed', true);
    }
  }

  const savedDictHotkey = await store.get('dictHotkey');
  if (savedDictHotkey) {
    dictHotkeyDisplay.textContent = savedDictHotkey;
    dictHotkeyDisplay.classList.add('hotkey-combo');
    dictHotkeyClear.style.display = '';

    try {
      await invoke('register_dict_hotkey', { shortcutStr: savedDictHotkey });
    } catch (err) {
      console.error('Dictionary hotkey registration failed:', err);
    }
  }
}

function setupEventListeners() {
  // Minimize button
  document.getElementById('titlebar-minimize').addEventListener('click', () => {
    getCurrentWindow().minimize();
  });

  // Close button — show prompt modal
  const closeModal = document.getElementById('close-modal');
  document.getElementById('titlebar-close').addEventListener('click', () => {
    closeModal.style.display = '';
  });

  // Modal: Minimize to Tray
  document.getElementById('modal-tray').addEventListener('click', async () => {
    closeModal.style.display = 'none';
    await invoke('hide_to_tray');
  });

  // Modal: Quit
  document.getElementById('modal-quit').addEventListener('click', async () => {
    closeModal.style.display = 'none';
    await invoke('quit_app');
  });

  // Modal: Cancel
  document.getElementById('modal-cancel').addEventListener('click', () => {
    closeModal.style.display = 'none';
  });

  // Close modal on overlay click
  closeModal.addEventListener('click', (e) => {
    if (e.target === closeModal) closeModal.style.display = 'none';
  });

  // Mic selection
  micSelect.addEventListener('change', async () => {
    const deviceId = micSelect.value;
    await store.set('audioDeviceId', deviceId);
    try {
      await invoke('set_audio_device', { deviceId });
      setStatus('Mic updated', false);
    } catch (err) {
      console.error('Failed to set device:', err);
      setStatus('Device failed', true);
    }
  });

  // Mode selection
  modeSelect.addEventListener('change', async () => {
    const mode = modeSelect.value;
    await store.set('transcriptionMode', mode);
    await toggleApiKeyVisibility(mode);
    maybeHideSubDeps();
    updateSubGenerateBtn();
  });

  // API key — saved to Windows Credential Manager (never plaintext on disk)
  apikeyInput.addEventListener('change', async () => {
    if (modeSelect.value === 'azure-mai') {
      await invoke('save_azure_api_key', { key: apikeyInput.value }).catch(console.error);
    } else {
      await invoke('save_api_key', { key: apikeyInput.value }).catch(console.error);
    }
    setStatus('Key saved', false);
  });

  azureEndpointInput.addEventListener('change', async () => {
    await store.set('azureMaiEndpoint', azureEndpointInput.value.trim());
    setStatus('Endpoint saved', false);
  });

  openrouterKeyInput.addEventListener('change', async () => {
    await invoke('save_openrouter_api_key', { key: openrouterKeyInput.value }).catch(console.error);
    setStatus(openrouterKeyInput.value.trim() ? 'OpenRouter key saved' : 'OpenRouter key cleared', false);
    updateSubSummarizeBtn();
  });

  // API key visibility toggle
  apikeyToggle.addEventListener('click', () => {
    const isPassword = apikeyInput.type === 'password';
    apikeyInput.type = isPassword ? 'text' : 'password';
  });

  openrouterKeyToggle.addEventListener('click', () => {
    const isPassword = openrouterKeyInput.type === 'password';
    openrouterKeyInput.type = isPassword ? 'text' : 'password';
  });

  // Hotkey recorder
  hotkeyBtn.addEventListener('click', startHotkeyRecording);

  // Dictionary hotkey recorder
  dictHotkeyBtn.addEventListener('click', startDictHotkeyRecording);
  dictHotkeyClear.addEventListener('click', clearDictHotkey);

  // History clear
  historyClear.addEventListener('click', () => {
    clearHistory();
  });

  // Dictionary
  dictAddBtn.addEventListener('click', addDictTerm);
  dictInput.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') addDictTerm();
  });

  // Theme toggle
  themeToggle.addEventListener('change', () => {
    setTheme(themeToggle.checked ? 'dark' : 'light');
  });
}

// ── API Key / Local Whisper Section Toggle ─────────────
async function toggleApiKeyVisibility(mode) {
  const isAzureMai = mode === 'azure-mai';
  const usesApiKey = mode === 'groq' || isAzureMai;

  apikeySection.style.display = usesApiKey ? '' : 'none';
  azureConfigSection.style.display = isAzureMai ? '' : 'none';
  sectionLocalWhisper.style.display = mode === 'local' ? '' : 'none';

  await loadProviderSettings(mode);

  if (mode === 'local') {
    // Status only — never block settings/mode UI on model load.
    whisperStatusText.innerHTML = '<span class="spinner-inline"></span> Checking model\u2026';
    whisperDownloadBtn.style.display = 'none';
    whisperLoadBtn.style.display = 'none';

    await restoreAccelerationPreference();
    updateAccelerationUi();
    await checkWhisperModelStatus();
    // Background warm-up keeps first PTT snappy without freezing the UI.
    scheduleWhisperWarmup(400);
  } else {
    cancelWhisperWarmup();
    // Unload whisper model when switching away from local mode
    if (whisperModelLoaded || whisperLoadPromise) {
      invoke('unload_whisper_model').then(() => {
        whisperModelLoaded = false;
        whisperLoadPromise = null;
        console.log('[Whisper] Model unloaded (switched away from local mode)');
      }).catch(err => console.warn('[Whisper] Unload failed:', err));
    }
  }
}

function getSelectedAcceleration() {
  const value = accelerationSelect?.value || 'cuda';
  return value === 'vulkan' ? 'vulkan' : 'cuda';
}

async function restoreAccelerationPreference() {
  if (!accelerationSelect || !store) return;
  const saved = await store.get('localAcceleration');
  if (saved === 'vulkan' || saved === 'cuda') {
    accelerationSelect.value = saved;
  }
}

function updateAccelerationUi() {
  const acceleration = getSelectedAcceleration();
  if (sectionCudaRuntime) {
    sectionCudaRuntime.style.display = acceleration === 'cuda' ? '' : 'none';
  }
  if (sectionVulkanRuntime) {
    sectionVulkanRuntime.style.display = acceleration === 'vulkan' ? '' : 'none';
  }
  if (acceleration === 'cuda') {
    checkCudaRuntimeStatus();
  } else if (vulkanStatusText) {
    vulkanStatusText.textContent = 'Uses system GPU drivers (Vulkan 1.2+)';
    vulkanStatusText.classList.add('ready');
  }
}

async function onAccelerationChange() {
  const acceleration = getSelectedAcceleration();
  if (store) {
    await store.set('localAcceleration', acceleration);
  }
  updateAccelerationUi();

  // Backend switch requires a different sidecar process.
  cancelWhisperWarmup();
  if (whisperModelLoaded || whisperLoadPromise) {
    try {
      await invoke('unload_whisper_model');
    } catch (err) {
      console.warn('[Whisper] Unload before backend switch failed:', err);
    }
    whisperModelLoaded = false;
    whisperLoadPromise = null;
  }

  if (modeSelect.value === 'local') {
    await checkWhisperModelStatus();
    // Defer so the select interaction stays responsive while Vulkan/CUDA starts.
    scheduleWhisperWarmup(500);
  }
}

async function loadProviderSettings(mode) {
  if (mode === 'azure-mai') {
    apikeyLabel.textContent = 'Foundry API Key';
    apikeyInput.placeholder = 'Paste Foundry API key';
    apikeyInput.value = await invoke('load_azure_api_key').catch(() => null) || '';
    azureEndpointInput.value = await store.get('azureMaiEndpoint') || '';
    return;
  }

  if (mode === 'groq') {
    apikeyLabel.textContent = 'Groq API Key';
    apikeyInput.placeholder = 'Paste Groq API key';
    apikeyInput.value = await invoke('load_api_key').catch(() => null) || '';
    return;
  }

  apikeyInput.value = '';
}

async function loadOpenRouterKey() {
  openrouterKeyInput.value = await invoke('load_openrouter_api_key').catch(() => null) || '';
  updateSubSummarizeBtn();
}

async function maybeLoadOpenRouterKey() {
  if (!openrouterKeyInput.value) {
    await loadOpenRouterKey();
  }
}

/**
 * Auto-load the whisper model if downloaded. Safe to call repeatedly; concurrent
 * callers share one in-flight promise.
 */
async function autoLoadWhisperIfReady({ interactive = false } = {}) {
  if (whisperModelLoaded) return true;
  try {
    const downloaded = await invoke('check_whisper_model');
    if (!downloaded) return false;
    await loadWhisperModel({ interactive });
    return whisperModelLoaded;
  } catch (err) {
    console.warn('[AutoLoad] Could not auto-load whisper model:', err);
    return false;
  }
}

let whisperWarmupTimer = null;

function cancelWhisperWarmup() {
  if (whisperWarmupTimer != null) {
    clearTimeout(whisperWarmupTimer);
    whisperWarmupTimer = null;
  }
}

/**
 * Schedule a background model warm-up after the UI has settled.
 * Vulkan device init is especially costly; delaying it avoids startup freezes.
 */
function scheduleWhisperWarmup(delayMs = 1500) {
  cancelWhisperWarmup();
  if (modeSelect.value !== 'local') return;

  whisperWarmupTimer = setTimeout(() => {
    whisperWarmupTimer = null;
    if (modeSelect.value !== 'local' || whisperModelLoaded || whisperLoadPromise) return;
    void autoLoadWhisperIfReady({ interactive: false });
  }, delayMs);
}

// ── Hotkey Recording ───────────────────────────────────
let recordingHotkey = false;

function startHotkeyRecording() {
  recordHotkeyCombo({
    btn: hotkeyBtn,
    display: hotkeyDisplay,
    command: 'register_hotkey',
    storeKey: 'hotkey',
    onSaved: () => setStatus('Hotkey set', false),
  });
}

function startDictHotkeyRecording() {
  recordHotkeyCombo({
    btn: dictHotkeyBtn,
    display: dictHotkeyDisplay,
    command: 'register_dict_hotkey',
    storeKey: 'dictHotkey',
    onSaved: () => {
      dictHotkeyClear.style.display = '';
      setStatus('Dictionary hotkey set', false);
    },
  });
}

function recordHotkeyCombo({ btn, display, command, storeKey, onSaved }) {
  if (recordingHotkey) return;
  recordingHotkey = true;

  btn.classList.add('recording');
  display.textContent = 'Press a key combo…';

  const handler = async (e) => {
    e.preventDefault();
    e.stopPropagation();

    // Build Tauri shortcut string
    const parts = [];
    if (e.ctrlKey) parts.push('Control');
    if (e.altKey) parts.push('Alt');
    if (e.shiftKey) parts.push('Shift');
    if (e.metaKey) parts.push('Super');

    // Ignore standalone modifier keys
    const modifierKeys = ['Control', 'Alt', 'Shift', 'Meta'];
    if (modifierKeys.includes(e.key)) return;

    // Map the key
    let key = mapKeyToTauri(e.code);
    parts.push(key);

    const shortcut = parts.join('+');
    const previousHotkey = await store.get(storeKey);

    document.removeEventListener('keydown', handler, true);
    recordingHotkey = false;
    btn.classList.remove('recording');

    display.textContent = shortcut;
    display.classList.add('hotkey-combo');

    try {
      await invoke(command, { shortcutStr: shortcut });
      await store.set(storeKey, shortcut);
      onSaved?.();
    } catch (err) {
      console.error(`Failed to register hotkey (${command}):`, err);
      setStatus('Invalid hotkey', true);

      // Restore the prior binding on failure.
      if (previousHotkey) {
        display.textContent = previousHotkey;
        display.classList.add('hotkey-combo');
        try {
          await invoke(command, { shortcutStr: previousHotkey });
        } catch (restoreErr) {
          console.error('Failed to restore previous hotkey:', restoreErr);
        }
      } else {
        display.textContent = 'Not set';
        display.classList.remove('hotkey-combo');
      }
    }
  };

  document.addEventListener('keydown', handler, true);
}

async function clearDictHotkey() {
  try {
    await invoke('unregister_dict_hotkey');
  } catch (err) {
    console.error('Failed to unregister dictionary hotkey:', err);
  }
  await store.delete('dictHotkey');
  dictHotkeyDisplay.textContent = 'Click to record';
  dictHotkeyDisplay.classList.remove('hotkey-combo');
  dictHotkeyClear.style.display = 'none';
  setStatus('Dictionary hotkey cleared', false);
}

function mapKeyToTauri(code) {
  if (code.startsWith('Key')) return code.slice(3);
  if (code.startsWith('Digit')) return code.slice(5);
  if (code === 'Space') return 'Space';
  if (code === 'Backquote') return '`';
  if (code === 'Minus') return '-';
  if (code === 'Equal') return '=';
  if (code === 'BracketLeft') return '[';
  if (code === 'BracketRight') return ']';
  if (code === 'Backslash') return '\\';
  if (code === 'Semicolon') return ';';
  if (code === 'Quote') return "'";
  if (code === 'Comma') return ',';
  if (code === 'Period') return '.';
  if (code === 'Slash') return '/';
  if (code.startsWith('F') && !isNaN(code.slice(1))) return code;
  return code;
}

// ── Hotkey Event Handling ──────────────────────────────
async function setupHotkeyListener() {
  await listen('hotkey-down', async () => {
    if (isRecording || isProcessing) return;
    isRecording = true;
    setStatus('Listening…', false, true);

    // Show throbber
    try {
      await emit('throbber-listen', {});
      await invoke('show_throbber');
    } catch (err) {
      console.error('Throbber show failed:', err);
    }

    const mode = modeSelect.value;

    if (mode === 'azure-mai') {
      warmAzureRestConnection();
    }

    // Kick off model load while the user is speaking if warm-up hasn't finished.
    if (mode === 'local' && !whisperModelLoaded) {
      void autoLoadWhisperIfReady({ interactive: false });
    }

    if (mode === 'webspeech') {
      try {
        startWebSpeech();
      } catch (err) {
        console.error('WebSpeech start failed:', err);
        isRecording = false;
        setStatus('Speech failed', true);
        try { await invoke('hide_throbber'); } catch {}
      }
    } else if (mode === 'local' || mode === 'azure-mai') {
      try {
        startWavRecording();
      } catch (err) {
        console.error('WAV recording start failed:', err);
        isRecording = false;
        setStatus('Mic failed', true);
        try { await invoke('hide_throbber'); } catch {}
      }
    } else {
      try {
        startMediaRecording();
      } catch (err) {
        console.error('Media recording start failed:', err);
        isRecording = false;
        setStatus('Mic failed', true);
        try { await invoke('hide_throbber'); } catch {}
      }
    }
  });

  await listen('hotkey-up', async () => {
    if (!isRecording) return;
    isRecording = false;
    isProcessing = true;

    // Hide throbber
    try {
      await invoke('hide_throbber');
    } catch (err) {
      console.error('Throbber hide failed:', err);
    }

    try {
      const mode = modeSelect.value;

      if (mode === 'webspeech') {
        await stopWebSpeech();
      } else if (mode === 'local' || mode === 'azure-mai') {
        await stopWavRecording();
      } else {
        await stopMediaRecording();
      }
    } catch (err) {
      console.error('Hotkey-up handling failed:', err);
      setStatus('Transcription failed', true);
    } finally {
      isProcessing = false;
    }
  });

  await listen('dict-hotkey-down', () => {
    void captureSelectionToDictionary();
  });
}

// ── Add Selection to Dictionary ────────────────────────
let capturingSelection = false;

// How long the throbber stays up while confirming a dictionary change.
const DICT_CONFIRM_MS = 1600;

// Reuse the throbber overlay to confirm a dictionary change near the cursor,
// so the user gets feedback even when the main window isn't focused.
async function showDictThrobberConfirm(kind, text) {
  try {
    // Send the confirmation content first; the throbber window is persistent
    // (created hidden at startup) so its listener is already active.
    await emit('throbber-confirm', { kind, text });
    await invoke('show_throbber');
    setTimeout(() => {
      invoke('hide_throbber').catch((err) =>
        console.error('Throbber hide failed:', err)
      );
    }, DICT_CONFIRM_MS);
  } catch (err) {
    console.error('Throbber confirm failed:', err);
  }
}

async function captureSelectionToDictionary() {
  if (capturingSelection) return;
  capturingSelection = true;

  try {
    const captured = await invoke('capture_selected_text', {});
    const term = String(captured || '').replace(/\s+/g, ' ').trim();

    if (!term) {
      setStatus('No text selected', true);
      return;
    }

    // Guard against accidental large selections — dictionary terms are words/phrases
    if (term.length > 100) {
      setStatus('Selection too long', true);
      return;
    }

    const result = addTermToDictionary(term);
    const shortTerm = term.length > 24 ? term.slice(0, 24) + '…' : term;

    if (result === 'added') {
      setStatus(`Added "${shortTerm}"`, false);
      void showDictThrobberConfirm('added', shortTerm);
    } else if (result === 'duplicate') {
      setStatus(`"${shortTerm}" already in dictionary`, false);
      void showDictThrobberConfirm('duplicate', shortTerm);
    }
  } catch (err) {
    console.error('Capture selection failed:', err);
    setStatus('Capture failed', true);
  } finally {
    capturingSelection = false;
  }
}

// ── Web Speech API Mode ────────────────────────────────
let webSpeechFinalTranscript = '';
let webSpeechInterimTranscript = '';

function startWebSpeech() {
  const SpeechRecognition = window.SpeechRecognition || window.webkitSpeechRecognition;

  if (!SpeechRecognition) {
    setStatus('Not supported', true);
    isRecording = false;
    return;
  }

  // Kill any orphaned instance from a previous cycle
  if (speechRecognition) {
    try { speechRecognition.abort(); } catch {}
    speechRecognition = null;
  }

  webSpeechFinalTranscript = '';
  webSpeechInterimTranscript = '';

  speechRecognition = new SpeechRecognition();
  speechRecognition.continuous = true;
  speechRecognition.interimResults = true;
  speechRecognition.lang = 'en-US';

  speechRecognition.onresult = (event) => {
    let interim = '';
    for (let i = event.resultIndex; i < event.results.length; i++) {
      const result = event.results[i];
      if (result.isFinal) {
        webSpeechFinalTranscript += result[0].transcript;
      } else {
        interim += result[0].transcript;
      }
    }
    webSpeechInterimTranscript = interim;
  };

  speechRecognition.onerror = (event) => {
    console.error('Speech recognition error:', event.error);
    if (event.error !== 'aborted') {
      setStatus(`Error: ${event.error}`, true);
    }
  };

  speechRecognition.onend = async () => {
    const text = webSpeechFinalTranscript.trim() || webSpeechInterimTranscript.trim();

    if (text) {
      setStatus('Pasting…', false);
      try {
        const finalText = normalizeTextForOutput(await maybeCorrectGrammar(text));
        await invoke('paste_text', { text: finalText });
        addHistoryEntry(finalText);
        setStatus('Done', false);
      } catch (err) {
        console.error('Paste failed:', err);
        setStatus('Paste failed', true);
      }
    } else {
      setStatus('No speech', false);
    }

    speechRecognition = null;
    webSpeechFinalTranscript = '';
    webSpeechInterimTranscript = '';
  };

  speechRecognition.start();
}

function stopWebSpeech() {
  return new Promise((resolve) => {
    if (!speechRecognition) {
      resolve();
      return;
    }
    const origOnEnd = speechRecognition.onend;
    speechRecognition.onend = async (...args) => {
      if (origOnEnd) await origOnEnd(...args);
      resolve();
    };
    speechRecognition.stop();
  });
}

// ── MediaRecorder / Groq Mode ──────────────────────────
// Tracks peak RMS during recording — shared by cloud & local pipelines.
let recordingPeakRms = 0;
let rmsAnalyserNode = null;
let rmsAudioCtx = null;

// ~-40 dBFS — matches the local whisper-worker threshold
const RMS_SILENCE_THRESHOLD = 0.01;

function startMediaRecording() {
  // Stop any leftover stream from a previous cycle
  if (mediaRecorder && mediaRecorder.stream) {
    try { mediaRecorder.stream.getTracks().forEach(t => t.stop()); } catch {}
  }
  mediaRecorder = null;
  audioChunks = [];
  recordingPeakRms = 0;

  navigator.mediaDevices.getUserMedia({ audio: true })
    .then(stream => {
      // Set up Web Audio analyser to track RMS energy during recording
      try {
        rmsAudioCtx = new AudioContext();
        const source = rmsAudioCtx.createMediaStreamSource(stream);
        rmsAnalyserNode = rmsAudioCtx.createAnalyser();
        rmsAnalyserNode.fftSize = 256;
        source.connect(rmsAnalyserNode);

        const buf = new Float32Array(rmsAnalyserNode.fftSize);
        // Use setInterval instead of requestAnimationFrame —
        // rAF stops firing when the window is hidden (hide-to-tray),
        // leaving recordingPeakRms at 0 and blocking all audio.
        const rmsInterval = setInterval(() => {
          if (!rmsAnalyserNode) { clearInterval(rmsInterval); return; }
          rmsAnalyserNode.getFloatTimeDomainData(buf);
          const rms = Math.sqrt(buf.reduce((s, v) => s + v * v, 0) / buf.length);
          if (rms > recordingPeakRms) recordingPeakRms = rms;
        }, 50);
      } catch (e) {
        console.warn('RMS analyser unavailable:', e);
      }

      mediaRecorder = new MediaRecorder(stream, { mimeType: 'audio/webm;codecs=opus' });

      mediaRecorder.ondataavailable = (e) => {
        if (e.data.size > 0) {
          audioChunks.push(e.data);
        }
      };

      mediaRecorder.start(100);
    })
    .catch(err => {
      console.error('Media recording failed:', err);
      setStatus('Mic denied', true);
      isRecording = false;
    });
}

function formatTimingMs(start) {
  return `${(performance.now() - start).toFixed(1)}ms`;
}

function logTranscriptionTiming(scope, step, start, extra = '') {
  const suffix = extra ? ` ${extra}` : '';
  console.info(`[Timing][${scope}] ${step}: ${formatTimingMs(start)}${suffix}`);
}

function stopRmsAnalyser() {
  rmsAnalyserNode = null;
  if (rmsAudioCtx) {
    rmsAudioCtx.close().catch(() => {});
    rmsAudioCtx = null;
  }
}

async function stopMediaRecording() {
  if (!mediaRecorder || mediaRecorder.state === 'inactive') {
    setStatus('No recording', false);
    return;
  }

  return new Promise((resolve) => {
    mediaRecorder.onstop = async () => {
      const totalTimer = performance.now();
      console.info(`[Timing][groq] stop begin chunks=${audioChunks.length}`);

      try { mediaRecorder.stream.getTracks().forEach(t => t.stop()); } catch {}

      if (audioChunks.length === 0) {
        setStatus('No audio', false);
        mediaRecorder = null;
        resolve();
        return;
      }

      stopRmsAnalyser();

      // RMS energy gate — skip transcription if audio was near-silent.
      // Applies to both cloud and local pipelines consistently.
      if (recordingPeakRms < RMS_SILENCE_THRESHOLD) {
        console.debug(`[RMS] Skipping — peak RMS ${recordingPeakRms.toFixed(5)} below threshold`);
        setStatus('No speech', false);
        mediaRecorder = null;
        resolve();
        return;
      }

      setStatus('Transcribing…', false, true);

      const encodeTimer = performance.now();
      const blob = new Blob(audioChunks, { type: 'audio/webm' });
      const arrayBuffer = await blob.arrayBuffer();
      const bytes = new Uint8Array(arrayBuffer);

      let binary = '';
      for (let i = 0; i < bytes.length; i++) {
        binary += String.fromCharCode(bytes[i]);
      }
      const base64 = btoa(binary);
      logTranscriptionTiming('groq', 'webm blob+base64', encodeTimer, `bytes=${bytes.length} b64=${base64.length}`);

      const apiKey = apikeyInput.value;
      if (!apiKey) {
        setStatus('No API key', true);
        mediaRecorder = null;
        resolve();
        return;
      }

      // Build initial prompt from dictionary terms
      const initialPrompt = getDictionaryPrompt();
      const language = liveLangSelect.value === 'auto' ? null : liveLangSelect.value;

      try {
        const invokeTimer = performance.now();
        const text = await invoke('transcribe_audio', {
          audioBase64: base64,
          apiKey: apiKey,
          initialPrompt: initialPrompt,
          language,
        });
        logTranscriptionTiming('groq', 'invoke transcribe_audio', invokeTimer);

        if (text && text.trim()) {
          let finalText = text.trim();
          const grammarTimer = performance.now();
          finalText = normalizeTextForOutput(await maybeCorrectGrammar(finalText));
          logTranscriptionTiming('groq', 'grammar cleanup', grammarTimer);
          setStatus('Pasting…', false);
          const pasteTimer = performance.now();
          await invoke('paste_text', { text: finalText });
          logTranscriptionTiming('groq', 'paste_text', pasteTimer);
          addHistoryEntry(finalText);
          setStatus('Done', false);
        } else {
          setStatus('No speech', false);
        }
      } catch (err) {
        console.error('Transcription failed:', err);
        setStatus('Transcribe failed', true);
      }

      logTranscriptionTiming('groq', 'total stop-to-ready', totalTimer);
      mediaRecorder = null;
      resolve();
    };

    mediaRecorder.stop();
  });
}

// ── Dictionary (tombstone-aware) ───────────────────────
// Storage format: versioned live values and tombstones with operation timestamps.
// Tombstones (deleted[]) ensure deletions propagate across synced devices.
const DICT_KEY = 'annotate_dictionary';

/**
 * Read the full dictionary store from localStorage.
 * Auto-migrates from the old string[] format.
 */
function getDictStore() {
  try {
    return normalizeDictionaryStore(JSON.parse(
      localStorage.getItem(DICT_KEY) || '{"terms":[],"deleted":[]}',
    ));
  } catch {
    return normalizeDictionaryStore(null);
  }
}

function saveDictStore(store) {
  localStorage.setItem(DICT_KEY, JSON.stringify(store));
}

/** Convenience: just the terms (for prompt building, rendering, etc.) */
function getDictionary() {
  return getDictStore().terms.map(term => term.value);
}

function getDictionaryPrompt() {
  const terms = getDictionary();
  if (terms.length === 0) return '';
  return terms.join(', ');
}

function addDictTerm() {
  const term = dictInput.value.trim();
  if (!term) return;

  addTermToDictionary(term);
  dictInput.value = '';
}

/**
 * Add a term to the dictionary store. Returns:
 *  'added'     — term was added
 *  'duplicate' — term already exists
 *  'invalid'   — empty/blank term
 */
function addTermToDictionary(rawTerm) {
  const term = String(rawTerm || '').trim();
  if (!term) return 'invalid';

  const store = getDictStore();
  const key = normalizeKey(term);

  // Avoid duplicates
  if (store.terms.some(item => normalizeKey(item.value) === key)) {
    return 'duplicate';
  }

  store.terms.push({ value: term, updatedAt: Date.now() });
  store.terms.sort((a, b) => a.value.localeCompare(b.value, undefined, { sensitivity: 'base' }));

  // Lift tombstone if re-adding a previously deleted term
  store.deleted = store.deleted.filter(item => normalizeKey(item.value) !== key);

  saveDictStore(store);
  renderDictionary();
  sync.scheduleSyncAfterChange();
  return 'added';
}

function removeDictTerm(index) {
  const store = getDictStore();
  const removed = store.terms.splice(index, 1)[0];

  if (removed) {
    const key = normalizeKey(removed.value);
    store.deleted = store.deleted.filter(item => normalizeKey(item.value) !== key);
    store.deleted.push({ value: removed.value, deletedAt: Date.now() });
  }

  saveDictStore(store);
  renderDictionary();
  sync.scheduleSyncAfterChange();
}

function renderDictionary() {
  const terms = getDictionary();
  dictList.innerHTML = '';

  dictCount.textContent = terms.length > 0 ? terms.length : '';

  if (terms.length === 0) {
    dictList.innerHTML = '<li class="dict-empty">No terms added yet</li>';
    return;
  }

  terms.forEach((term, index) => {
    const li = document.createElement('li');
    li.className = 'dict-item';

    const textSpan = document.createElement('span');
    textSpan.className = 'dict-item-text';
    textSpan.textContent = term;

    const removeBtn = document.createElement('button');
    removeBtn.className = 'dict-item-remove';
    removeBtn.textContent = 'Remove';
    removeBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      removeDictTerm(index);
    });

    li.appendChild(textSpan);
    li.appendChild(removeBtn);
    dictList.appendChild(li);
  });
}

// ── Status ─────────────────────────────────────────────
function setStatus(message, isError = false, isActive = false) {
  statusText.textContent = message;
  statusDot.className = 'status-dot';

  if (isError) {
    statusDot.classList.add('error');
  } else if (isActive) {
    statusDot.classList.add('active');
  }

  // Auto-clear after 3s (if not active)
  if (!isActive) {
    setTimeout(() => {
      statusText.textContent = 'Ready';
      statusDot.className = 'status-dot';
    }, 3000);
  }
}

function normalizeTextForOutput(text) {
  return text
    .normalize('NFC')
    .replace(/\u00A0/g, ' ')
    .replace(/[\u200B-\u200D\uFEFF]/g, '')
    .replace(/\r\n|\r|\n/g, '\r\n');
}

// ── History (tombstone-aware) ──────────────────────────
// Storage format: entries and tombstones with operation timestamps.
// Tombstones (deleted[]) ensure deletions propagate across synced devices.
const HISTORY_KEY = 'annotate_history';
const HISTORY_MAX = 50;

/**
 * Read the full history store from localStorage.
 * Auto-migrates from the old flat array format.
 */
function getHistoryStore() {
  try {
    return normalizeHistoryStore(JSON.parse(
      localStorage.getItem(HISTORY_KEY) || '{"entries":[],"deleted":[]}',
    ));
  } catch {
    return normalizeHistoryStore(null);
  }
}

function saveHistoryStore(store) {
  localStorage.setItem(HISTORY_KEY, JSON.stringify(store));
}

/** Convenience: just the entries (for rendering, etc.) */
function getHistory() {
  return getHistoryStore().entries;
}

function addHistoryEntry(text) {
  const store = getHistoryStore();
  const key = normalizeKey(text);
  const now = Date.now();

  store.entries = store.entries.filter(entry => normalizeKey(entry.text) !== key);
  store.entries.unshift({ text, time: now, updatedAt: now });
  if (store.entries.length > HISTORY_MAX) store.entries.length = HISTORY_MAX;

  // Lift tombstone if re-adding a previously deleted entry
  store.deleted = store.deleted.filter(entry => normalizeKey(entry.text) !== key);

  saveHistoryStore(store);
  renderHistory();
  sync.scheduleSyncAfterChange();
}

function removeHistoryEntry(index) {
  const store = getHistoryStore();
  const removed = store.entries.splice(index, 1)[0];

  if (removed) {
    const key = normalizeKey(removed.text);
    store.deleted = store.deleted.filter(entry => normalizeKey(entry.text) !== key);
    store.deleted.push({ text: removed.text, time: removed.time, deletedAt: Date.now() });
  }

  saveHistoryStore(store);
  renderHistory();
  sync.scheduleSyncAfterChange();
}

function clearHistory() {
  const store = getHistoryStore();
  const deletedAt = Date.now();

  // Tombstone every entry so the clear propagates across devices
  for (const entry of store.entries) {
    const key = normalizeKey(entry.text);
    store.deleted = store.deleted.filter(item => normalizeKey(item.text) !== key);
    store.deleted.push({ text: entry.text, time: entry.time, deletedAt });
  }
  store.entries = [];

  saveHistoryStore(store);
  renderHistory();
  sync.scheduleSyncAfterChange();
}

function renderHistory() {
  const history = getHistory();
  historyList.innerHTML = '';

  historyCount.textContent = history.length > 0 ? history.length : '';

  if (history.length === 0) {
    historyList.innerHTML = '<li class="history-empty">No transcriptions yet</li>';
    historyClear.style.display = 'none';
    return;
  }

  historyClear.style.display = '';

  history.forEach((entry, index) => {
    const li = document.createElement('li');
    li.className = 'history-item';

    const timeSpan = document.createElement('span');
    timeSpan.className = 'history-item-time';
    timeSpan.textContent = formatTime(entry.time);

    const textSpan = document.createElement('span');
    textSpan.className = 'history-item-text';
    textSpan.textContent = entry.text;

    const actionsSpan = document.createElement('span');
    actionsSpan.className = 'history-item-actions';

    const copyBtn = document.createElement('button');
    copyBtn.className = 'history-item-copy';
    copyBtn.textContent = 'Copy';
    copyBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      navigator.clipboard.writeText(normalizeTextForOutput(entry.text));
      copyBtn.textContent = 'Copied!';
      setTimeout(() => { copyBtn.textContent = 'Copy'; }, 1200);
    });

    const removeBtn = document.createElement('button');
    removeBtn.className = 'history-item-remove';
    removeBtn.textContent = 'Delete';
    removeBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      removeHistoryEntry(index);
    });

    actionsSpan.appendChild(copyBtn);
    actionsSpan.appendChild(removeBtn);

    li.appendChild(timeSpan);
    li.appendChild(textSpan);
    li.appendChild(actionsSpan);
    historyList.appendChild(li);
  });
}

function formatTime(timestamp) {
  const d = new Date(timestamp);
  const now = new Date();
  const isToday = d.toDateString() === now.toDateString();
  const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  if (isToday) return time;
  return d.toLocaleDateString([], { month: 'short', day: 'numeric' }) + ' ' + time;
}

// ── Theme ──────────────────────────────────────────────
async function loadTheme() {
  const savedTheme = await store.get('theme');
  const theme = savedTheme || 'light';
  document.documentElement.setAttribute('data-theme', theme);
  themeToggle.checked = theme === 'dark';
}

async function setTheme(theme) {
  // Add transition class for smooth switching
  document.documentElement.classList.add('theme-transitioning');
  document.documentElement.setAttribute('data-theme', theme);
  await store.set('theme', theme);

  // Remove transition class after animation completes
  setTimeout(() => {
    document.documentElement.classList.remove('theme-transitioning');
  }, 450);
}

// ── Local Whisper Model Management ─────────────────────
let whisperModelLoaded = false;
/** @type {Promise<boolean> | null} */
let whisperLoadPromise = null;

async function checkWhisperModelStatus() {
  try {
    const downloaded = await invoke('check_whisper_model');
    const acceleration = getSelectedAcceleration();
    const backendLabel = acceleration === 'vulkan' ? 'Vulkan' : 'CUDA';
    if (downloaded) {
      if (whisperModelLoaded) {
        whisperStatusText.textContent = `Model ready (${backendLabel})`;
        whisperStatusText.classList.add('ready');
        whisperDownloadBtn.style.display = 'none';
        whisperLoadBtn.style.display = 'none';
      } else if (whisperLoadPromise) {
        whisperStatusText.innerHTML =
          `<span class="spinner-inline"></span> Loading model (${backendLabel})\u2026`;
        whisperStatusText.classList.remove('ready');
        whisperDownloadBtn.style.display = 'none';
        whisperLoadBtn.style.display = 'none';
      } else {
        whisperStatusText.textContent = `Model downloaded \u2014 warming ${backendLabel}\u2026`;
        whisperStatusText.classList.remove('ready');
        whisperDownloadBtn.style.display = 'none';
        // Keep manual load available if background warm-up hasn't started yet.
        whisperLoadBtn.style.display = '';
      }
    } else {
      whisperStatusText.textContent = 'Model not downloaded (~600 MB)';
      whisperStatusText.classList.remove('ready');
      whisperDownloadBtn.style.display = '';
      whisperLoadBtn.style.display = 'none';
    }
  } catch (err) {
    console.error('Check whisper model failed:', err);
    whisperStatusText.textContent = '\u2717 Error checking model';
  }
}

async function downloadWhisperModel() {
  whisperDownloadBtn.style.display = 'none';
  whisperProgressWrap.style.display = '';
  whisperStatusText.textContent = 'Downloading model…';

  // Listen for progress events
  const unlisten = await listen('whisper-download-progress', (event) => {
    const [downloaded, total] = event.payload;
    if (total > 0) {
      const pct = Math.round((downloaded / total) * 100);
      whisperProgressFill.style.width = pct + '%';
      whisperProgressText.textContent = pct + '%';
    } else {
      const mb = (downloaded / 1048576).toFixed(1);
      whisperProgressText.textContent = mb + ' MB';
    }
  });

  try {
    await invoke('download_whisper_model');
    whisperStatusText.innerHTML = '<span class="spinner-inline"></span> Loading model\u2026';
    whisperProgressWrap.style.display = 'none';
    unlisten();
    await loadWhisperModel();
  } catch (err) {
    console.error('Download failed:', err);
    whisperStatusText.textContent = '\u2717 Download failed';
    whisperDownloadBtn.style.display = '';
    whisperProgressWrap.style.display = 'none';
    unlisten();
  }
}

/**
 * Load the local whisper sidecar/model. Concurrent callers share one promise.
 * @param {{ interactive?: boolean }} [opts]
 */
async function loadWhisperModel({ interactive = true } = {}) {
  if (whisperModelLoaded) return true;
  if (whisperLoadPromise) return whisperLoadPromise;

  const acceleration = getSelectedAcceleration();
  const backendLabel = acceleration === 'vulkan' ? 'Vulkan' : 'CUDA';

  whisperLoadPromise = (async () => {
    whisperLoadBtn.style.display = 'none';
    whisperStatusText.innerHTML =
      `<span class="spinner-inline"></span> Loading model (${backendLabel})\u2026`;
    whisperStatusText.classList.remove('ready');
    if (interactive) {
      setStatus(`Loading Whisper (${backendLabel})\u2026`, false, true);
    }

    try {
      await invoke('load_whisper_model', { acceleration });
      whisperModelLoaded = true;
      whisperStatusText.textContent = `Model ready (${backendLabel})`;
      whisperStatusText.classList.add('ready');
      if (interactive || modeSelect.value === 'local') {
        setStatus(`Whisper ready (${backendLabel})`, false);
      }
      return true;
    } catch (err) {
      console.error('Load failed:', err);
      whisperStatusText.textContent = '\u2717 Load failed: ' + err;
      whisperLoadBtn.style.display = '';
      if (interactive) {
        setStatus('Whisper load failed', true);
      }
      return false;
    } finally {
      whisperLoadPromise = null;
    }
  })();

  return whisperLoadPromise;
}

// ── CUDA Runtime Management ────────────────────────────
let cudaRuntimeReady = false;

async function checkCudaRuntimeStatus() {
  try {
    const result = await invoke('check_cuda_runtime');
    if (result.available) {
      cudaRuntimeReady = true;
      cudaStatusText.textContent = 'CUDA runtime ready';
      cudaStatusText.classList.add('ready');
      cudaDownloadBtn.style.display = 'none';
    } else if (result.copied_from_toolkit > 0 && result.available) {
      cudaRuntimeReady = true;
      cudaStatusText.textContent = 'CUDA runtime copied from toolkit';
      cudaStatusText.classList.add('ready');
      cudaDownloadBtn.style.display = 'none';
    } else {
      cudaRuntimeReady = false;
      cudaStatusText.classList.remove('ready');
      const missingList = result.missing.join(', ');
      if (result.has_toolkit) {
        cudaStatusText.textContent = `Missing DLLs (toolkit copy failed): ${missingList}`;
      } else {
        cudaStatusText.textContent = `Missing: ${missingList} (~350 MB download)`;
      }
      cudaDownloadBtn.style.display = '';
    }
  } catch (err) {
    console.error('CUDA check failed:', err);
    cudaStatusText.textContent = 'Error checking CUDA runtime';
  }
}

async function downloadCudaRuntime() {
  cudaDownloadBtn.style.display = 'none';
  cudaProgressWrap.style.display = '';
  cudaStatusText.textContent = 'Downloading CUDA runtime from NVIDIA…';

  const unlisten = await listen('cuda-download-progress', (event) => {
    const [downloaded, total] = event.payload;
    if (total > 0) {
      const pct = Math.round((downloaded / total) * 100);
      cudaProgressFill.style.width = pct + '%';
      cudaProgressText.textContent = pct + '%';
    } else {
      const mb = (downloaded / 1048576).toFixed(1);
      cudaProgressText.textContent = mb + ' MB';
    }
  });

  try {
    await invoke('download_cuda_runtime');
    cudaRuntimeReady = true;
    cudaStatusText.textContent = 'CUDA runtime ready';
    cudaStatusText.classList.add('ready');
    cudaProgressWrap.style.display = 'none';
    cudaDownloadBtn.style.display = 'none';
    unlisten();
  } catch (err) {
    console.error('CUDA download failed:', err);
    cudaStatusText.textContent = 'Download failed: ' + err;
    cudaDownloadBtn.style.display = '';
    cudaProgressWrap.style.display = 'none';
    unlisten();
  }
}

// Wire up model, acceleration, and CUDA buttons
document.addEventListener('DOMContentLoaded', () => {
  whisperDownloadBtn.addEventListener('click', downloadWhisperModel);
  whisperLoadBtn.addEventListener('click', () => {
    void loadWhisperModel({ interactive: true });
  });
  cudaDownloadBtn.addEventListener('click', downloadCudaRuntime);
  if (accelerationSelect) {
    accelerationSelect.addEventListener('change', () => {
      void onAccelerationChange();
    });
  }
});

// ── WAV Recording (for Local Whisper) ──────────────────
// Records raw PCM audio and encodes it as a 16kHz mono 16-bit WAV
let wavAudioContext = null;
let wavStream = null;
let wavScriptNode = null;
let wavSourceNode = null;
let wavBuffers = [];
let lastAzureRestActivityAt = Number.NEGATIVE_INFINITY;

function warmAzureRestConnection() {
  const endpoint = azureEndpointInput.value.trim();
  const now = performance.now();

  // reqwest keeps idle connections pooled. Avoid an extra request while the
  // existing HTTP/2 connection is likely to still be usable.
  if (!endpoint || now - lastAzureRestActivityAt < 60_000) return;
  lastAzureRestActivityAt = now;

  const timer = performance.now();
  invoke('warm_azure_transcription', { endpoint })
    .then(() => logTranscriptionTiming('azure-mai', 'connection warm-up', timer))
    .catch(err => console.warn('[Azure MAI] Connection warm-up failed:', err));
}

function startWavRecording() {
  // Clean up any previous recording
  cleanupWavRecording();
  wavBuffers = [];

  navigator.mediaDevices.getUserMedia({ audio: { sampleRate: 16000, channelCount: 1 } })
    .then(stream => {
      wavStream = stream;
      wavAudioContext = new (window.AudioContext || window.webkitAudioContext)({ sampleRate: 16000 });
      wavSourceNode = wavAudioContext.createMediaStreamSource(stream);

      // ScriptProcessor to capture raw PCM
      const bufferSize = 4096;
      wavScriptNode = wavAudioContext.createScriptProcessor(bufferSize, 1, 1);
      wavScriptNode.onaudioprocess = (e) => {
        const data = e.inputBuffer.getChannelData(0);
        wavBuffers.push(new Float32Array(data));
      };

      wavSourceNode.connect(wavScriptNode);
      wavScriptNode.connect(wavAudioContext.destination);
    })
    .catch(err => {
      console.error('WAV recording failed:', err);
      setStatus('Mic denied', true);
      isRecording = false;
    });
}

async function stopWavRecording() {
  const mode = modeSelect.value;
  const timingScope = mode === 'azure-mai' ? 'azure-mai' : 'local';
  const totalTimer = performance.now();
  console.info(`[Timing][${timingScope}] stop begin buffers=${wavBuffers.length}`);

  if (!wavAudioContext || wavBuffers.length === 0) {
    cleanupWavRecording();
    setStatus('No audio', false);
    return;
  }

  // Stop recording
  if (wavScriptNode) {
    wavScriptNode.disconnect();
    wavScriptNode.onaudioprocess = null;
  }
  if (wavSourceNode) wavSourceNode.disconnect();
  if (wavStream) wavStream.getTracks().forEach(t => t.stop());

  setStatus('Transcribing…', false, true);

  const mergeTimer = performance.now();
  // Merge all buffers
  const totalLength = wavBuffers.reduce((sum, b) => sum + b.length, 0);
  const merged = new Float32Array(totalLength);
  let offset = 0;
  for (const buf of wavBuffers) {
    merged.set(buf, offset);
    offset += buf.length;
  }
  logTranscriptionTiming(timingScope, 'merge PCM buffers', mergeTimer, `samples=${totalLength}`);

  const rmsTimer = performance.now();
  if (calculatePeakRms(merged) < RMS_SILENCE_THRESHOLD) {
    logTranscriptionTiming(timingScope, 'RMS gate', rmsTimer, 'silent');
    cleanupWavRecording();
    setStatus('No speech', false);
    return;
  }
  logTranscriptionTiming(timingScope, 'RMS gate', rmsTimer, 'speech');

  // Encode as 16-bit PCM WAV
  const encodeTimer = performance.now();
  const sampleRate = wavAudioContext ? wavAudioContext.sampleRate : 16000;
  const wavBytes = encodeWav(merged, sampleRate);
  logTranscriptionTiming(timingScope, 'encode WAV', encodeTimer, `bytes=${wavBytes.byteLength} sampleRate=${sampleRate}`);

  cleanupWavRecording();

  // Convert to base64
  const base64Timer = performance.now();
  const bytes = new Uint8Array(wavBytes);
  let binary = '';
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  const base64 = btoa(binary);
  logTranscriptionTiming(timingScope, 'base64 encode', base64Timer, `b64=${base64.length}`);

  // Build initial prompt from dictionary terms
  const initialPrompt = getDictionaryPrompt();

  try {
    let text = '';

    if (mode === 'azure-mai') {
      const apiKey = apikeyInput.value || await invoke('load_azure_api_key').catch(() => null);
      const endpoint = azureEndpointInput.value.trim();
      const language = liveLangSelect.value === 'auto' ? null : liveLangSelect.value;

      if (!apiKey) {
        setStatus('No API key', true);
        return;
      }
      if (!endpoint) {
        setStatus('No endpoint', true);
        return;
      }

      const invokeTimer = performance.now();
      try {
        text = await invoke('transcribe_audio_azure', {
          audioBase64: base64,
          apiKey,
          endpoint,
          initialPrompt: initialPrompt || null,
          language,
        });
      } finally {
        lastAzureRestActivityAt = performance.now();
      }
      logTranscriptionTiming('azure-mai', 'REST transcription', invokeTimer);
    } else {
      // Ensure the sidecar is up (deferred warm-up may still be in flight).
      const ready = await autoLoadWhisperIfReady({ interactive: true });
      if (!ready) {
        setStatus('Whisper not ready', true);
        return;
      }
      const invokeTimer = performance.now();
      text = await invoke('transcribe_audio_local', {
        audioBase64: base64,
        initialPrompt: initialPrompt || null
      });
      logTranscriptionTiming('local', 'invoke transcribe_audio_local', invokeTimer);
    }

    if (text && text.trim()) {
      let finalText = text.trim();
      const grammarTimer = performance.now();
      finalText = normalizeTextForOutput(await maybeCorrectGrammar(finalText));
      logTranscriptionTiming(timingScope, 'grammar cleanup', grammarTimer);
      setStatus('Pasting…', false);
      const pasteTimer = performance.now();
      await invoke('paste_text', {
        text: finalText,
        releaseDelayMs: mode === 'azure-mai' ? 0 : null,
      });
      logTranscriptionTiming(timingScope, 'paste_text', pasteTimer);
      addHistoryEntry(finalText);
      setStatus('Done', false);
    } else {
      setStatus('No speech', false);
    }
  } catch (err) {
    console.error('WAV transcription failed:', err);
    setStatus('Transcribe failed', true);
  } finally {
    logTranscriptionTiming(timingScope, 'total stop-to-ready', totalTimer);
  }
}

function calculatePeakRms(samples, windowSize = 512) {
  let peak = 0;

  for (let i = 0; i < samples.length; i += windowSize) {
    const end = Math.min(i + windowSize, samples.length);
    let sum = 0;
    for (let j = i; j < end; j++) {
      sum += samples[j] * samples[j];
    }
    const rms = Math.sqrt(sum / (end - i));
    if (rms > peak) peak = rms;
  }

  return peak;
}

function cleanupWavRecording() {
  if (wavScriptNode) {
    try { wavScriptNode.disconnect(); } catch {}
    wavScriptNode = null;
  }
  if (wavSourceNode) {
    try { wavSourceNode.disconnect(); } catch {}
    wavSourceNode = null;
  }
  if (wavStream) {
    try { wavStream.getTracks().forEach(t => t.stop()); } catch {}
    wavStream = null;
  }
  if (wavAudioContext) {
    try { wavAudioContext.close(); } catch {}
    wavAudioContext = null;
  }
  wavBuffers = [];
}

function encodeWav(samples, sampleRate) {
  const numChannels = 1;
  const bitsPerSample = 16;
  const bytesPerSample = bitsPerSample / 8;
  const blockAlign = numChannels * bytesPerSample;
  const dataLength = samples.length * bytesPerSample;
  const headerLength = 44;
  const buffer = new ArrayBuffer(headerLength + dataLength);
  const view = new DataView(buffer);

  // RIFF header
  writeString(view, 0, 'RIFF');
  view.setUint32(4, headerLength + dataLength - 8, true);
  writeString(view, 8, 'WAVE');

  // fmt chunk
  writeString(view, 12, 'fmt ');
  view.setUint32(16, 16, true);              // chunk size
  view.setUint16(20, 1, true);               // PCM format
  view.setUint16(22, numChannels, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * blockAlign, true); // byte rate
  view.setUint16(32, blockAlign, true);
  view.setUint16(34, bitsPerSample, true);

  // data chunk
  writeString(view, 36, 'data');
  view.setUint32(40, dataLength, true);

  // Write samples as 16-bit PCM
  let idx = headerLength;
  for (let i = 0; i < samples.length; i++) {
    let s = Math.max(-1, Math.min(1, samples[i]));
    s = s < 0 ? s * 0x8000 : s * 0x7FFF;
    view.setInt16(idx, s, true);
    idx += 2;
  }

  return buffer;
}

function writeString(view, offset, str) {
  for (let i = 0; i < str.length; i++) {
    view.setUint8(offset + i, str.charCodeAt(i));
  }
}

// ── Google Drive Sync UI ───────────────────────────────
function obfuscateEmail(email) {
  if (!email || !email.includes('@')) return email;
  const [local, domain] = email.split('@');
  const show = Math.min(3, local.length);
  return local.slice(0, show) + '***@' + domain;
}

async function initSyncUI() {
  const signedOutDiv = document.getElementById('sync-signed-out');
  const signedInDiv  = document.getElementById('sync-signed-in');
  const waitingDiv   = document.getElementById('sync-waiting');
  const signInBtn    = document.getElementById('sync-signin-btn');
  const signOutBtn   = document.getElementById('sync-signout-btn');
  const syncNowBtn   = document.getElementById('sync-now-btn');
  const avatarEl     = document.getElementById('sync-user-avatar');
  const nameEl       = document.getElementById('sync-user-name');
  const emailEl      = document.getElementById('sync-user-email');

  // Set up callbacks
  sync.setSyncCallbacks({
    onStatus(status, detail) {
      const textEl = syncNowBtn.querySelector('.sync-now-text');

      if (status === 'syncing') {
        syncNowBtn.classList.add('syncing');
        syncNowBtn.classList.remove('synced-flash');
      } else if (status === 'synced') {
        syncNowBtn.classList.remove('syncing');
        syncNowBtn.classList.add('synced-flash');
        textEl.textContent = 'Synced';
        setTimeout(() => {
          // 1. Pin current width so we have a known start point
          const currentW = syncNowBtn.getBoundingClientRect().width;
          syncNowBtn.style.width = currentW + 'px';

          // 2. Fade out text
          textEl.style.opacity = '0';

          setTimeout(() => {
            // 3. Swap content while invisible
            syncNowBtn.classList.remove('synced-flash');
            textEl.textContent = 'Sync Now';

            // 4. Force reflow so browser registers the new content width
            void syncNowBtn.offsetWidth;

            // 5. Release the pinned width — CSS transition animates to natural size
            syncNowBtn.style.width = '';

            // 6. Fade text back in
            textEl.style.opacity = '1';
          }, 250);
        }, 15000);
      } else if (status === 'error') {
        syncNowBtn.classList.remove('syncing');
        textEl.textContent = 'Retry';
        setTimeout(() => { textEl.textContent = 'Sync Now'; }, 3000);
      } else {
        syncNowBtn.classList.remove('syncing', 'synced-flash');
        textEl.textContent = 'Sync Now';
      }
    },
    onSignIn(signedIn, user) {
      if (signedIn && user) {
        signedOutDiv.style.display = 'none';
        waitingDiv.style.display = 'none';
        signedInDiv.style.display = '';
        avatarEl.src = user.picture || '';
        avatarEl.style.display = user.picture ? '' : 'none';
        nameEl.textContent = user.name || 'User';
        emailEl.textContent = obfuscateEmail(user.email || '');
      } else {
        signedOutDiv.style.display = '';
        waitingDiv.style.display = 'none';
        signedInDiv.style.display = 'none';
      }
    }
  });

  const reopenBtn     = document.getElementById('sync-reopen-btn');
  const cancelAuthBtn = document.getElementById('sync-cancel-btn');

  // Show the "waiting for browser" panel and kick off the sign-in flow.
  // Returns when the flow completes, is cancelled, or times out.
  async function startSignIn() {
    signedOutDiv.style.display = 'none';
    waitingDiv.style.display = '';

    try {
      await sync.signIn();
      // onSignIn callback will show signed-in panel
    } catch (err) {
      const cancelled = err?.message?.includes('oauth_cancelled') ||
                        String(err).includes('oauth_cancelled');
      if (!cancelled) {
        console.error('Sign in error:', err);
      }
      // Return to signed-out panel on any failure/cancel
      waitingDiv.style.display = 'none';
      signedOutDiv.style.display = '';
    }
  }

  // Sign in button
  signInBtn.addEventListener('click', () => startSignIn());

  // "Open Again" — cancel the stale listener, start a fresh OAuth flow
  reopenBtn.addEventListener('click', async () => {
    await invoke('cancel_google_oauth'); // unblock any pending accept()
    // Small delay so Rust cleans up before we bind a new port
    await new Promise(r => setTimeout(r, 100));
    startSignIn();
  });

  // "Cancel" — unblock the Rust accept(), go back to sign-in screen
  cancelAuthBtn.addEventListener('click', async () => {
    await invoke('cancel_google_oauth');
    waitingDiv.style.display = 'none';
    signedOutDiv.style.display = '';
  });

  // Sign out button
  signOutBtn.addEventListener('click', async () => {
    await sync.signOut();
  });

  // Sync now button — smooth spinner transition
  syncNowBtn.addEventListener('click', async () => {
    if (syncNowBtn.classList.contains('syncing')) return;
    try {
      await sync.syncNow();
    } catch (err) {
      console.error('Manual sync failed:', err);
      syncNowBtn.classList.remove('syncing');
      const textEl = syncNowBtn.querySelector('.sync-now-text');
      textEl.textContent = 'Retry';
      setTimeout(() => { textEl.textContent = 'Sync Now'; }, 3000);
    }
  });

  // Listen for sync data changes to re-render UI
  window.addEventListener('sync-data-changed', (e) => {
    renderDictionary();
    renderHistory();
  });

  // Initialize the sync module (restores tokens, triggers initial sync)
  await sync.initSync();
}

// ── Grammar Cleanup (GEC) ──────────────────────────────

async function initGrammarUI() {
  // Restore saved toggle state
  const savedGrammar = await store.get('grammarCleanupEnabled');
  const enabled = savedGrammar === true;
  grammarToggle.checked = enabled;
  gecModelSection.style.display = enabled ? 'block' : 'none';

  // Toggle change handler
  grammarToggle.addEventListener('change', async () => {
    const on = grammarToggle.checked;
    await store.set('grammarCleanupEnabled', on);
    gecModelSection.style.display = on ? 'block' : 'none';

    if (on) {
      await checkAndLoadGecModel();
    } else {
      // Unload GEC model when toggle is turned off
      if (gecModelLoaded) {
        gecStatusText.textContent = 'Unloading model\u2026';
        try {
          await invoke('unload_gec_model');
          gecModelLoaded = false;
          gecStatusText.textContent = '';
          gecStatusText.classList.remove('ready');
          console.log('[GEC] Model unloaded');
        } catch (err) {
          console.warn('[GEC] Unload failed:', err);
        }
      }
    }
  });

  // Download button
  gecDownloadBtn.addEventListener('click', () => downloadGecModel());

  // If already enabled, check model status
  if (enabled) {
    await checkAndLoadGecModel();
  }
}

async function checkAndLoadGecModel() {
  try {
    const exists = await invoke('check_gec_model');
    if (exists) {
      gecStatusText.innerHTML = '<span class="spinner-inline"></span> Loading model…';
      gecDownloadBtn.style.display = 'none';
      try {
        await invoke('load_gec_model');
        gecStatusText.textContent = 'Model ready';
        gecStatusText.classList.add('ready');
        gecModelLoaded = true;
      } catch (err) {
        console.error('GEC load failed:', err);
        gecStatusText.textContent = '\u2717 Load failed';
      }
    } else {
      gecStatusText.textContent = 'Model not downloaded (~105 MB)';
      gecDownloadBtn.style.display = '';
    }
  } catch (err) {
    console.error('GEC check failed:', err);
    gecStatusText.textContent = '\u2717 Check failed';
  }
}

async function downloadGecModel() {
  gecDownloadBtn.style.display = 'none';
  gecProgressWrap.style.display = '';
  gecStatusText.textContent = 'Downloading…';

  // Listen for progress events
  const unlisten = await listen('gec-download-progress', (event) => {
    const [downloaded, total] = event.payload;
    if (total > 0) {
      const pct = Math.round((downloaded / total) * 100);
      gecProgressFill.style.width = `${pct}%`;
      gecProgressText.textContent = `${pct}%`;
    }
  });

  try {
    await invoke('download_gec_model');
    unlisten();
    gecProgressWrap.style.display = 'none';
    gecStatusText.innerHTML = '<span class="spinner-inline"></span> Loading model\u2026';

    await invoke('load_gec_model');
    gecStatusText.textContent = 'Model ready';
    gecStatusText.classList.add('ready');
    gecModelLoaded = true;
  } catch (err) {
    unlisten();
    console.error('GEC download/load failed:', err);
    gecProgressWrap.style.display = 'none';
    gecStatusText.textContent = '\u2717 Download failed';
    gecDownloadBtn.style.display = '';
    gecDownloadBtn.textContent = 'Retry Download';
  }
}

/**
 * If grammar cleanup is enabled and the model is loaded,
 * run GECToR correction on the text. Otherwise return as-is.
 */
async function maybeCorrectGrammar(text) {
  if (!grammarToggle.checked || !gecModelLoaded) {
    return text;
  }

  try {
    setStatus('Cleaning grammar…', false, true);
    const corrected = await invoke('correct_grammar', { text });
    if (corrected && corrected.trim()) {
      console.log('[GEC] Corrected:', text, '→', corrected.trim());
      return corrected.trim();
    }
  } catch (err) {
    console.error('[GEC] Grammar correction failed:', err);
  }

  return text; // fallback: return original
}

// ── Subtitling ─────────────────────────────────────────

// State
let subSelectedFile = null;
let subSrtEntries = null;
let subGenerating = false;
let subSummarizing = false;

// Elements
const subFileZone = document.getElementById('sub-file-zone');
const subFileLabel = document.getElementById('sub-file-label');
const subFileName = document.getElementById('sub-file-name');
const subGenerateBtn = document.getElementById('sub-generate-btn');
const subSummarizeBtn = document.getElementById('sub-summarize-btn');
const subProgress = document.getElementById('sub-progress');
const subProgressFill = document.getElementById('sub-progress-fill');
const subProgressText = document.getElementById('sub-progress-text');
const subResult = document.getElementById('sub-result');
const subResultTitle = document.getElementById('sub-result-title');
const subSummary = document.getElementById('sub-summary');
const subSummaryText = document.getElementById('sub-summary-text');
const subSummaryCopyBtn = document.getElementById('sub-summary-copy-btn');
const subPreview = document.getElementById('sub-preview');
const subPreviewCount = document.getElementById('sub-preview-count');
const subSaveBtn = document.getElementById('sub-save-btn');
const subNewBtn = document.getElementById('sub-new-btn');
const subFfmpegStatus = document.getElementById('sub-dep-ffmpeg-status');
const subFfmpegDownloadBtn = document.getElementById('sub-ffmpeg-download-btn');
const subVadStatus = document.getElementById('sub-dep-vad-status');
const subVadDownloadBtn = document.getElementById('sub-vad-download-btn');
const subDeps = document.getElementById('sub-deps');
const subDesc = document.getElementById('sub-desc');
const subLangSelect = document.getElementById('sub-lang-select');
const subLangGroup = document.getElementById('sub-lang-group');
const liveLangSelect = document.getElementById('live-lang-select');

async function initSubtitlingUI() {
  // Check dependencies
  await checkSubDeps();

  // Restore saved language preferences
  const savedSubLang = await store.get('subtitleLanguage');
  if (savedSubLang && subLangSelect.querySelector(`option[value="${savedSubLang}"]`)) {
    subLangSelect.value = savedSubLang;
  }
  const savedLiveLang = await store.get('liveLanguage');
  if (savedLiveLang && liveLangSelect.querySelector(`option[value="${savedLiveLang}"]`)) {
    liveLangSelect.value = savedLiveLang;
  }

  // Persist on change
  subLangSelect.addEventListener('change', () => store.set('subtitleLanguage', subLangSelect.value));
  liveLangSelect.addEventListener('change', () => store.set('liveLanguage', liveLangSelect.value));

  // File picker
  subFileZone.addEventListener('click', pickSubFile);

  // Generate button
  subGenerateBtn.addEventListener('click', generateSubtitles);

  // Save button
  subSaveBtn.addEventListener('click', saveSrtFile);

  // Summary button
  subSummarizeBtn.addEventListener('click', summarizeSubtitles);
  subSummaryCopyBtn.addEventListener('click', copySubSummary);

  // New File button
  subNewBtn.addEventListener('click', resetSubtitling);

  // FFmpeg download
  subFfmpegDownloadBtn.addEventListener('click', async () => {
    subFfmpegDownloadBtn.style.display = 'none';
    subFfmpegStatus.textContent = 'Downloading...';
    try {
      await invoke('download_ffmpeg');
      subFfmpegStatus.textContent = 'Ready';
      subFfmpegStatus.classList.add('ready');
      maybeHideSubDeps();
      updateSubGenerateBtn();
    } catch (err) {
      subFfmpegStatus.textContent = 'Download failed';
      subFfmpegDownloadBtn.style.display = '';
      console.error('[Subtitle] FFmpeg download failed:', err);
    }
  });

  // VAD download
  subVadDownloadBtn.addEventListener('click', async () => {
    subVadDownloadBtn.style.display = 'none';
    subVadStatus.textContent = 'Downloading...';
    try {
      await invoke('download_vad_model');
      subVadStatus.textContent = 'Ready';
      subVadStatus.classList.add('ready');
      maybeHideSubDeps();
      updateSubGenerateBtn();
    } catch (err) {
      subVadStatus.textContent = 'Download failed';
      subVadDownloadBtn.style.display = '';
      console.error('[Subtitle] VAD download failed:', err);
    }
  });

  // Listen for progress events
  listen('subtitle-progress', (event) => {
    const p = event.payload;
    subProgress.style.display = '';
    subProgressText.textContent = p.message;

    if (p.stage === 'transcribe' && p.total > 0) {
      const pct = Math.round((p.current / p.total) * 100);
      subProgressFill.style.width = pct + '%';
    } else if (p.stage === 'format') {
      subProgressFill.style.width = '100%';
    } else {
      // Indeterminate for extract/read/vad
      subProgressFill.style.width = '30%';
    }
  });

  // Listen for ffmpeg/vad download progress
  listen('ffmpeg-download-progress', (event) => {
    const [downloaded, total] = event.payload;
    if (total > 0) {
      const pct = Math.round((downloaded / total) * 100);
      subFfmpegStatus.textContent = `Downloading... ${pct}%`;
    }
  });

  listen('vad-download-progress', (event) => {
    const [downloaded, total] = event.payload;
    if (total > 0) {
      const pct = Math.round((downloaded / total) * 100);
      subVadStatus.textContent = `Downloading... ${pct}%`;
    }
  });
}

async function checkSubDeps() {
  // FFmpeg
  try {
    const ffmpegReady = await invoke('check_ffmpeg');
    if (ffmpegReady) {
      subFfmpegStatus.textContent = 'Ready';
      subFfmpegStatus.classList.add('ready');
    } else {
      subFfmpegStatus.textContent = 'Not installed';
      subFfmpegDownloadBtn.style.display = '';
    }
  } catch (err) {
    subFfmpegStatus.textContent = 'Error';
    console.error('[Subtitle] FFmpeg check failed:', err);
  }

  // VAD Model
  try {
    const vadReady = await invoke('check_vad_model');
    if (vadReady) {
      subVadStatus.textContent = 'Ready';
      subVadStatus.classList.add('ready');
    } else {
      subVadStatus.textContent = 'Not installed';
      subVadDownloadBtn.style.display = '';
    }
  } catch (err) {
    subVadStatus.textContent = 'Error';
    console.error('[Subtitle] VAD check failed:', err);
  }

  maybeHideSubDeps();
  updateSubGenerateBtn();
}

// Hide deps section when the selected subtitle engine has its dependencies.
function maybeHideSubDeps() {
  const ffmpegOk = subFfmpegStatus.classList.contains('ready');
  const vadOk = subVadStatus.classList.contains('ready');
  if (ffmpegOk && vadOk) {
    subDeps.style.display = 'none';
  } else {
    subDeps.style.display = '';
  }
}

async function pickSubFile() {
  if (subGenerating) return;

  try {
    // Use Tauri dialog to pick a file
    const { open } = window.__TAURI__.dialog;
    const selected = await open({
      multiple: false,
      filters: [{
        name: 'Media Files',
        extensions: ['mp4', 'mkv', 'avi', 'mov', 'webm', 'mp3', 'wav', 'flac', 'ogg', 'aac', 'm4a', 'wma']
      }]
    });

    if (selected) {
      subSelectedFile = selected;
      const name = selected.split(/[\\/]/).pop();
      subFileName.textContent = name;
      subFileZone.classList.add('has-file');

      // Reset result
      subResult.style.display = 'none';
      subSrtEntries = null;
      resetSubSummary();

      updateSubGenerateBtn();
      updateSubSummarizeBtn();
    }
  } catch (err) {
    console.error('[Subtitle] File picker failed:', err);
  }
}

function updateSubGenerateBtn() {
  const ffmpegOk = subFfmpegStatus.classList.contains('ready');
  const vadOk = subVadStatus.classList.contains('ready');
  const hasFile = !!subSelectedFile;
  const notGenerating = !subGenerating;

  // Check engine-specific readiness
  const engine = modeSelect.value;
  let engineOk = true;
  if (engine === 'groq') {
    // Need API key — it's loaded by now
    engineOk = true; // API key is fetched during generate
  }

  subGenerateBtn.disabled = !(ffmpegOk && vadOk && hasFile && notGenerating && engineOk);
}

function updateSubSummarizeBtn() {
  const hasEntries = !!subSrtEntries && subSrtEntries.length > 0;
  const hasKey = !!openrouterKeyInput?.value?.trim();
  subSummarizeBtn.disabled = subGenerating || subSummarizing || !hasEntries || !hasKey;
  subSummarizeBtn.title = hasKey
    ? ''
    : 'Add an OpenRouter API key in Settings to enable summaries';
}

function resetSubSummary() {
  subSummarizing = false;
  subSummary.style.display = 'none';
  subSummaryText.textContent = '';
  subSummaryCopyBtn.textContent = 'Copy';
  updateSubSummarizeBtn();
}

async function copySubSummary() {
  const summary = subSummaryText.textContent.trim();
  if (!summary) return;

  try {
    await navigator.clipboard.writeText(summary);
    subSummaryCopyBtn.textContent = 'Copied!';
    setTimeout(() => {
      subSummaryCopyBtn.textContent = 'Copy';
    }, 1200);
  } catch (err) {
    console.error('[Subtitle] Summary copy failed:', err);
  }
}

async function summarizeSubtitles() {
  if (!subSrtEntries || subSrtEntries.length === 0 || subSummarizing) return;

  subSummarizing = true;
  subSummarizeBtn.textContent = 'Summarizing...';
  subProgress.style.display = '';
  subProgressFill.style.width = '30%';
  subProgressText.textContent = 'Summarizing subtitles...';
  updateSubSummarizeBtn();

  try {
    const summary = await invoke('summarize_subtitles', { entries: subSrtEntries });
    const trimmed = (summary || '').trim();
    if (!trimmed) {
      throw new Error('Summary was empty');
    }

    subSummaryText.textContent = trimmed;
    subSummary.style.display = '';
    subSummaryCopyBtn.textContent = 'Copy';
    subProgressText.textContent = 'Summary ready';
    subProgressFill.style.width = '100%';
    subProgress.style.display = 'none';
  } catch (err) {
    subProgressText.textContent = 'Error: ' + (err.message || err);
    subProgressFill.style.width = '100%';
    console.error('[Subtitle] Summary failed:', err);
  } finally {
    subSummarizing = false;
    subSummarizeBtn.textContent = 'Summarize';
    updateSubSummarizeBtn();
  }
}

async function generateSubtitles() {
  if (!subSelectedFile || subGenerating) return;

  subGenerating = true;
  subGenerateBtn.disabled = true;
  subGenerateBtn.textContent = 'Generating...';
  subProgress.style.display = '';
  subProgressFill.style.width = '0%';
  subProgressText.textContent = 'Starting...';
  subResult.style.display = 'none';
  resetSubSummary();

  try {
    const selectedMode = modeSelect.value;
    const engine = selectedMode === 'local'
      ? 'local'
      : selectedMode === 'azure-mai'
        ? 'azure-mai'
        : 'groq';

    // Get API key if using a cloud engine
    let apiKey = null;
    let azureEndpoint = null;
    if (engine === 'groq') {
      try {
        apiKey = apikeyInput.value || await invoke('load_api_key');
      } catch (e) {
        throw new Error('Groq API key not configured. Set it in Settings.');
      }
    } else if (engine === 'azure-mai') {
      try {
        apiKey = apikeyInput.value || await invoke('load_azure_api_key');
      } catch (e) {
        throw new Error('Foundry API key not configured. Set it in Settings.');
      }

      azureEndpoint = azureEndpointInput.value.trim()
        || await store.get('azureMaiEndpoint');
      if (!azureEndpoint) {
        throw new Error('Azure Foundry endpoint not configured. Set it in Settings.');
      }
    }

    await maybeLoadOpenRouterKey();

    // Get dictionary terms as prompt
    let prompt = null;
    const dict = await store.get('dictionary');
    if (dict) {
      const terms = Object.entries(dict)
        .filter(([_, v]) => !v.deleted)
        .map(([k]) => k);
      if (terms.length > 0) {
        prompt = terms.join(', ');
      }
    }

    const subLang = subLangSelect.value;
    const entries = await invoke('generate_subtitles', {
      filePath: subSelectedFile,
      engine: engine,
      apiKey: apiKey,
      azureEndpoint: azureEndpoint,
      prompt: prompt,
      language: subLang === 'auto' ? null : subLang,
    });

    subSrtEntries = entries;
    updateSubSummarizeBtn();

    if (entries.length === 0) {
      subProgressText.textContent = 'No speech detected in the audio.';
      subProgressFill.style.width = '100%';
    } else {
      // Show result view
      const fileName = subSelectedFile.split(/[\\/]/).pop().replace(/\.[^.]+$/, '');
      subResultTitle.textContent = fileName;
      subResultTitle.title = fileName; // tooltip for full name

      const srtText = await invoke('format_srt_preview', { entries });
      subPreview.textContent = srtText;
      subPreviewCount.textContent = entries.length + ' entries';

      // Hide setup UI, show result
      subFileZone.style.display = 'none';
      subDeps.style.display = 'none';
      subGenerateBtn.style.display = 'none';
      subDesc.style.display = 'none';
      subLangGroup.style.display = 'none';
      subProgress.style.display = 'none';
      subResult.style.display = '';
      updateSubSummarizeBtn();
    }
  } catch (err) {
    subProgressText.textContent = 'Error: ' + (err.message || err);
    subProgressFill.style.width = '100%';
    console.error('[Subtitle] Generation failed:', err);
  } finally {
    subGenerating = false;
    subGenerateBtn.textContent = 'Generate Subtitles';
    updateSubGenerateBtn();
    updateSubSummarizeBtn();
  }
}

function resetSubtitling() {
  // Clear state
  subSelectedFile = null;
  subSrtEntries = null;
  subGenerating = false;
  subSummarizing = false;

  // Restore setup UI
  subFileZone.style.display = '';
  subFileZone.classList.remove('has-file');
  subFileName.textContent = '';
  subDesc.style.display = '';
  subLangGroup.style.display = '';
  subGenerateBtn.style.display = '';
  subGenerateBtn.textContent = 'Generate Subtitles';
  subProgress.style.display = 'none';
  subProgressFill.style.width = '0%';
  maybeHideSubDeps();
  resetSubSummary();

  // Hide result
  subResult.style.display = 'none';
  subPreview.textContent = '';
  subPreviewCount.textContent = '';
  subResultTitle.textContent = '';

  updateSubGenerateBtn();
  updateSubSummarizeBtn();
}

async function saveSrtFile() {
  if (!subSrtEntries || subSrtEntries.length === 0) return;

  try {
    const { save } = window.__TAURI__.dialog;

    // Derive default filename from source file
    const baseName = subSelectedFile
      ? subSelectedFile.split(/[\\/]/).pop().replace(/\.[^.]+$/, '')
      : 'subtitles';

    const outputPath = await save({
      defaultPath: baseName + '.srt',
      filters: [{
        name: 'SRT Subtitle',
        extensions: ['srt']
      }]
    });

    if (outputPath) {
      await invoke('save_srt_file', {
        entries: subSrtEntries,
        outputPath: outputPath,
      });
      subProgressText.textContent = 'Saved to ' + outputPath.split(/[\\/]/).pop();
      subProgress.style.display = '';
      subProgressFill.style.width = '100%';
    }
  } catch (err) {
    console.error('[Subtitle] Save failed:', err);
  }
}

// ── Boot ───────────────────────────────────────────────
document.addEventListener('DOMContentLoaded', init);
