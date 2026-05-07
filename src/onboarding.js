const { invoke } = window.__TAURI__.core;
const { emit } = window.__TAURI__.event;
const { load } = window.__TAURI__.store;

const micSelect = document.getElementById('onboarding-mic-select');
const modelSelect = document.getElementById('onboarding-model-select');
const localSection = document.getElementById('onboarding-local-section');
const accelerationSelect = document.getElementById('onboarding-acceleration-select');
const apiSection = document.getElementById('onboarding-api-section');
const apiInput = document.getElementById('onboarding-api-input');
const apiToggle = document.getElementById('onboarding-api-toggle');
const saveBtn = document.getElementById('onboarding-save');
const skipBtn = document.getElementById('onboarding-skip');
const summary = document.getElementById('onboarding-summary');
const explanationTitle = document.getElementById('onboarding-explanation-title');
const explanationBody = document.getElementById('onboarding-explanation-body');
const explanationSteps = document.getElementById('onboarding-explanation-steps');

let store = null;

const MODEL_OPTIONS = {
  webspeech: [
    {
      value: 'webspeech-default',
      label: 'WebSpeech (browser default)'
    }
  ],
  groq: [
    {
      value: 'whisper-large-v3-turbo',
      label: 'Whisper Large v3 Turbo'
    },
    {
      value: 'whisper-large-v3',
      label: 'Whisper Large v3'
    },
    {
      value: 'distil-whisper-large-v3-en',
      label: 'Distil Whisper Large v3 EN'
    }
  ],
  local: [
    {
      value: 'ggml-large-v3-turbo-q5_0',
      label: 'Local Whisper Large v3 Turbo Q5'
    }
  ]
};

const ONBOARDING_COPY = {
  webspeech: {
    title: 'Fastest start',
    body: 'WebSpeech uses the browser speech service and needs no API key, model download, or GPU runtime.',
    steps: [
      'Uses your selected microphone, or the system default if you skip setup.',
      'Starts immediately after you set a hotkey.',
      'You can switch to Groq or local Whisper later in Settings.'
    ]
  },
  groq: {
    title: 'Cloud Whisper with Groq',
    body: 'Groq runs Whisper in the cloud. Annotate stores your API key in Windows Credential Manager, not plaintext settings.',
    steps: [
      'Go to console.groq.com and sign in or create an account.',
      'Open API Keys, create a new key, then paste it here.',
      'Keep the key private. You can skip now and add it later in Settings.'
    ]
  },
  local: {
    title: 'Offline local transcription',
    body: 'Local Whisper keeps audio on this machine. CUDA is wired today; Vulkan is available as a saved preference for future support.',
    steps: [
      'Choose CUDA for the current local acceleration path.',
      'Choose Vulkan only if you want the preference saved ahead of future support.',
      'The local model download and load controls are available in Settings.'
    ]
  }
};

function getModelKey(mode) {
  return `transcriptionModel:${mode}`;
}

function getDefaultModel(mode) {
  return MODEL_OPTIONS[mode]?.[0]?.value || '';
}

function getSelectedMode() {
  const checked = document.querySelector('input[name="onboarding-mode"]:checked');
  return checked ? checked.value : 'webspeech';
}

function setSelectedMode(mode) {
  const radio = document.querySelector(`input[name="onboarding-mode"][value="${mode}"]`);
  if (radio) {
    radio.checked = true;
  }
  updateForMode(mode);
}

function populateModelSelect(mode, selectedValue) {
  const options = MODEL_OPTIONS[mode] || MODEL_OPTIONS.webspeech;
  const selected = selectedValue && options.some(option => option.value === selectedValue)
    ? selectedValue
    : options[0].value;

  modelSelect.innerHTML = '';
  options.forEach(option => {
    const opt = document.createElement('option');
    opt.value = option.value;
    opt.textContent = option.label;
    modelSelect.appendChild(opt);
  });
  modelSelect.value = selected;
}

function getModelLabel(mode, value) {
  const option = MODEL_OPTIONS[mode]?.find(item => item.value === value) || MODEL_OPTIONS[mode]?.[0];
  return option?.label || 'WebSpeech';
}

function updateForMode(mode) {
  populateModelSelect(mode, modelSelect.value || getDefaultModel(mode));

  apiSection.style.display = mode === 'groq' ? '' : 'none';
  localSection.style.display = mode === 'local' ? '' : 'none';

  const copy = ONBOARDING_COPY[mode] || ONBOARDING_COPY.webspeech;
  const deviceLabel = micSelect.value
    ? micSelect.options[micSelect.selectedIndex]?.textContent
    : 'Default system audio';

  summary.textContent = `${deviceLabel} + ${getModelLabel(mode, modelSelect.value)}`;
  explanationTitle.textContent = copy.title;
  explanationBody.textContent = copy.body;
  explanationSteps.innerHTML = '';
  copy.steps.forEach(step => {
    const li = document.createElement('li');
    li.textContent = step;
    explanationSteps.appendChild(li);
  });
}

function fillDeviceSelect(devices, selectedValue = '') {
  micSelect.innerHTML = '';

  const defaultOpt = document.createElement('option');
  defaultOpt.value = '';
  defaultOpt.textContent = 'Default system audio';
  micSelect.appendChild(defaultOpt);

  devices.forEach(device => {
    const opt = document.createElement('option');
    opt.value = device.id;
    opt.textContent = device.name;
    micSelect.appendChild(opt);
  });

  micSelect.value = selectedValue || '';
}

async function loadAudioDevices() {
  try {
    const devices = await invoke('get_audio_devices');
    const savedDevice = await store.get('audioDeviceId');
    fillDeviceSelect(devices, savedDevice || '');
  } catch (err) {
    console.error('Failed to load devices:', err);
    fillDeviceSelect([], '');
  }
}

async function saveSettings() {
  const mode = getSelectedMode();
  const deviceId = micSelect.value;
  const model = modelSelect.value || getDefaultModel(mode);
  const acceleration = accelerationSelect.value || 'cuda';

  await store.set('audioDeviceId', deviceId);
  await store.set('transcriptionMode', mode);
  await store.set(getModelKey(mode), model);
  await store.set('localAcceleration', acceleration);
  await store.set('onboardingComplete', true);

  if (mode === 'groq' && apiInput.value.trim()) {
    await invoke('save_api_key', { key: apiInput.value.trim() });
  }

  await invoke('set_audio_device', { deviceId });
  await emit('onboarding-complete');
  await invoke('close_onboarding');
}

async function skipSetup() {
  await store.set('audioDeviceId', '');
  await store.set('transcriptionMode', 'webspeech');
  await store.set(getModelKey('webspeech'), getDefaultModel('webspeech'));
  await store.set('onboardingComplete', true);

  await invoke('set_audio_device', { deviceId: '' });
  await emit('onboarding-complete');
  await invoke('close_onboarding');
}

function setupEvents() {
  document.querySelectorAll('input[name="onboarding-mode"]').forEach(radio => {
    radio.addEventListener('change', () => updateForMode(getSelectedMode()));
  });

  micSelect.addEventListener('change', () => updateForMode(getSelectedMode()));
  modelSelect.addEventListener('change', () => updateForMode(getSelectedMode()));
  accelerationSelect.addEventListener('change', () => updateForMode(getSelectedMode()));

  apiToggle.addEventListener('click', () => {
    const isPassword = apiInput.type === 'password';
    apiInput.type = isPassword ? 'text' : 'password';
  });

  saveBtn.addEventListener('click', saveSettings);
  skipBtn.addEventListener('click', skipSetup);
}

async function init() {
  store = await load('settings.json', { autoSave: true });

  const savedTheme = await store.get('theme');
  document.documentElement.setAttribute('data-theme', savedTheme || 'light');

  await loadAudioDevices();

  const savedMode = await store.get('transcriptionMode');
  const mode = savedMode || 'webspeech';
  const savedModel = await store.get(getModelKey(mode));
  const savedAcceleration = await store.get('localAcceleration');
  const savedKey = await invoke('load_api_key').catch(() => null);

  if (savedAcceleration) {
    accelerationSelect.value = savedAcceleration;
  }
  if (savedKey) {
    apiInput.value = savedKey;
  }

  setSelectedMode(mode);
  populateModelSelect(mode, savedModel);
  updateForMode(mode);
  setupEvents();
}

document.addEventListener('DOMContentLoaded', init);
