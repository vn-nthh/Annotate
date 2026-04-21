const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { load } = window.__TAURI__.store;
const { getCurrentWindow } = window.__TAURI__.window;

// ── State ──────────────────────────────────────────────
let store = null;
let isRecording = false;
let mediaRecorder = null;
let audioChunks = [];
let speechRecognition = null;

// ── DOM Elements ───────────────────────────────────────
const micSelect = document.getElementById('mic-select');
const modeSelect = document.getElementById('mode-select');
const apikeySection = document.getElementById('section-apikey');
const apikeyInput = document.getElementById('apikey-input');
const apikeyToggle = document.getElementById('apikey-toggle');
const hotkeyBtn = document.getElementById('hotkey-btn');
const hotkeyDisplay = document.getElementById('hotkey-display');
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

// Tab navigation
const tabBtns = document.querySelectorAll('.tab-btn');
const tabPanels = document.querySelectorAll('.tab-panel');

// ── Initialize ─────────────────────────────────────────
async function init() {
  store = await load('settings.json', { autoSave: true });

  await loadTheme();
  setupTabNavigation();
  await loadAudioDevices();
  await loadSavedSettings();
  setupEventListeners();
  await setupHotkeyListener();
  renderHistory();
  renderDictionary();
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

  const savedMode = await store.get('transcriptionMode');
  if (savedMode) {
    modeSelect.value = savedMode;
  }
  toggleApiKeyVisibility(modeSelect.value);

  const savedKey = await store.get('groqApiKey');
  if (savedKey) {
    apikeyInput.value = savedKey;
  }

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
    toggleApiKeyVisibility(mode);
  });

  // API key
  apikeyInput.addEventListener('change', async () => {
    await store.set('groqApiKey', apikeyInput.value);
    setStatus('Key saved', false);
  });

  // API key visibility toggle
  apikeyToggle.addEventListener('click', () => {
    const isPassword = apikeyInput.type === 'password';
    apikeyInput.type = isPassword ? 'text' : 'password';
  });

  // Hotkey recorder
  hotkeyBtn.addEventListener('click', startHotkeyRecording);

  // History clear
  historyClear.addEventListener('click', () => {
    localStorage.removeItem('annotate_history');
    renderHistory();
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

// ── API Key Section Toggle ─────────────────────────────
function toggleApiKeyVisibility(mode) {
  const show = mode === 'groq';
  apikeySection.style.display = show ? '' : 'none';
}

// ── Hotkey Recording ───────────────────────────────────
let recordingHotkey = false;

function startHotkeyRecording() {
  if (recordingHotkey) return;
  recordingHotkey = true;

  hotkeyBtn.classList.add('recording');
  hotkeyDisplay.textContent = 'Press a key combo…';

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

    document.removeEventListener('keydown', handler, true);
    recordingHotkey = false;
    hotkeyBtn.classList.remove('recording');

    hotkeyDisplay.textContent = shortcut;
    hotkeyDisplay.classList.add('hotkey-combo');

    await store.set('hotkey', shortcut);

    try {
      await invoke('register_hotkey', { shortcutStr: shortcut });
      setStatus('Hotkey set', false);
    } catch (err) {
      console.error('Failed to register hotkey:', err);
      setStatus('Invalid hotkey', true);
    }
  };

  document.addEventListener('keydown', handler, true);
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
    if (isRecording) return;
    isRecording = true;
    setStatus('Listening…', false, true);

    // Show throbber
    try {
      await invoke('show_throbber');
    } catch (err) {
      console.error('Throbber show failed:', err);
    }

    const mode = modeSelect.value;

    if (mode === 'webspeech') {
      try {
        startWebSpeech();
      } catch (err) {
        console.error('WebSpeech start failed:', err);
        isRecording = false;
        setStatus('Speech failed', true);
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

    // Hide throbber
    try {
      await invoke('hide_throbber');
    } catch (err) {
      console.error('Throbber hide failed:', err);
    }

    const mode = modeSelect.value;

    if (mode === 'webspeech') {
      stopWebSpeech();
    } else {
      await stopMediaRecording();
    }
  });
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
        await invoke('paste_text', { text });
        addHistoryEntry(text);
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
  if (speechRecognition) {
    speechRecognition.stop();
  }
}

// ── MediaRecorder / Groq Mode ──────────────────────────
function startMediaRecording() {
  // Stop any leftover stream from a previous cycle
  if (mediaRecorder && mediaRecorder.stream) {
    try { mediaRecorder.stream.getTracks().forEach(t => t.stop()); } catch {}
  }
  mediaRecorder = null;
  audioChunks = [];

  navigator.mediaDevices.getUserMedia({ audio: true })
    .then(stream => {
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

async function stopMediaRecording() {
  if (!mediaRecorder || mediaRecorder.state === 'inactive') {
    setStatus('No recording', false);
    return;
  }

  return new Promise((resolve) => {
    mediaRecorder.onstop = async () => {
      try { mediaRecorder.stream.getTracks().forEach(t => t.stop()); } catch {}

      if (audioChunks.length === 0) {
        setStatus('No audio', false);
        mediaRecorder = null;
        resolve();
        return;
      }

      setStatus('Transcribing…', false, true);

      const blob = new Blob(audioChunks, { type: 'audio/webm' });
      const arrayBuffer = await blob.arrayBuffer();
      const bytes = new Uint8Array(arrayBuffer);

      let binary = '';
      for (let i = 0; i < bytes.length; i++) {
        binary += String.fromCharCode(bytes[i]);
      }
      const base64 = btoa(binary);

      const apiKey = apikeyInput.value;
      if (!apiKey) {
        setStatus('No API key', true);
        mediaRecorder = null;
        resolve();
        return;
      }

      // Build initial prompt from dictionary terms
      const initialPrompt = getDictionaryPrompt();

      try {
        const text = await invoke('transcribe_audio', {
          audioBase64: base64,
          apiKey: apiKey,
          initialPrompt: initialPrompt
        });

        if (text && text.trim()) {
          setStatus('Pasting…', false);
          await invoke('paste_text', { text: text.trim() });
          addHistoryEntry(text.trim());
          setStatus('Done', false);
        } else {
          setStatus('No speech', false);
        }
      } catch (err) {
        console.error('Transcription failed:', err);
        setStatus('Transcribe failed', true);
      }

      mediaRecorder = null;
      resolve();
    };

    mediaRecorder.stop();
  });
}

// ── Dictionary ─────────────────────────────────────────
const DICT_KEY = 'annotate_dictionary';

function getDictionary() {
  try {
    return JSON.parse(localStorage.getItem(DICT_KEY) || '[]');
  } catch {
    return [];
  }
}

function saveDictionary(terms) {
  localStorage.setItem(DICT_KEY, JSON.stringify(terms));
}

function getDictionaryPrompt() {
  const terms = getDictionary();
  if (terms.length === 0) return '';
  return terms.join(', ');
}

function addDictTerm() {
  const term = dictInput.value.trim();
  if (!term) return;

  const terms = getDictionary();
  // Avoid duplicates
  if (terms.some(t => t.toLowerCase() === term.toLowerCase())) {
    dictInput.value = '';
    return;
  }

  terms.push(term);
  saveDictionary(terms);
  dictInput.value = '';
  renderDictionary();
}

function removeDictTerm(index) {
  const terms = getDictionary();
  terms.splice(index, 1);
  saveDictionary(terms);
  renderDictionary();
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

// ── History ────────────────────────────────────────────
const HISTORY_KEY = 'annotate_history';
const HISTORY_MAX = 50;

function getHistory() {
  try {
    return JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]');
  } catch {
    return [];
  }
}

function addHistoryEntry(text) {
  const history = getHistory();
  history.unshift({ text, time: Date.now() });
  if (history.length > HISTORY_MAX) history.length = HISTORY_MAX;
  localStorage.setItem(HISTORY_KEY, JSON.stringify(history));
  renderHistory();
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

  history.forEach(entry => {
    const li = document.createElement('li');
    li.className = 'history-item';

    const timeSpan = document.createElement('span');
    timeSpan.className = 'history-item-time';
    timeSpan.textContent = formatTime(entry.time);

    const textSpan = document.createElement('span');
    textSpan.className = 'history-item-text';
    textSpan.textContent = entry.text;

    const copyBtn = document.createElement('button');
    copyBtn.className = 'history-item-copy';
    copyBtn.textContent = 'Copy';
    copyBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      navigator.clipboard.writeText(entry.text);
      copyBtn.textContent = 'Copied!';
      setTimeout(() => { copyBtn.textContent = 'Copy'; }, 1200);
    });

    li.appendChild(timeSpan);
    li.appendChild(textSpan);
    li.appendChild(copyBtn);
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

// ── Boot ───────────────────────────────────────────────
document.addEventListener('DOMContentLoaded', init);
