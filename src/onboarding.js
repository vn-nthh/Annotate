const { invoke } = window.__TAURI__.core;
const { emit } = window.__TAURI__.event;
const { load } = window.__TAURI__.store;

const micSelect = document.getElementById('onboarding-mic-select');
const modelSelect = document.getElementById('onboarding-model-select');
const modelSection = document.getElementById('onboarding-model-section');
const localSection = document.getElementById('onboarding-local-section');
const accelerationSelect = document.getElementById('onboarding-acceleration-select');
const apiSection = document.getElementById('onboarding-api-section');
const apiLabel = document.getElementById('onboarding-api-label');
const apiInput = document.getElementById('onboarding-api-input');
const apiToggle = document.getElementById('onboarding-api-toggle');
const azureSection = document.getElementById('onboarding-azure-section');
const azureEndpointInput = document.getElementById('onboarding-azure-endpoint-input');
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
  'azure-mai': [
    {
      value: 'azure-mai',
      label: 'MAI transcription'
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
  'azure-mai': {
    title: 'Microsoft MAI transcription',
    body: 'Microsoft MAI transcription runs through your Azure AI Foundry project. Annotate stores your API key in Windows Credential Manager.',
    steps: [
      'Use the Foundry API key for your speech project.',
      'Paste the project endpoint from your Azure AI Foundry project.',
      'Dictionary terms are sent as Azure phrase-list biasing.'
    ]
  },
  local: {
    title: 'Offline local transcription',
    body: 'Local Whisper keeps audio on this machine and can run on CUDA or Vulkan.',
    steps: [
      'Choose CUDA for NVIDIA GPUs with the CUDA runtime.',
      'Choose Vulkan for AMD, Intel, or NVIDIA GPUs using your existing drivers.',
      'Download and load the local model from Settings after setup.'
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

  const isAzureMai = mode === 'azure-mai';
  apiSection.style.display = mode === 'groq' || isAzureMai ? '' : 'none';
  azureSection.style.display = isAzureMai ? '' : 'none';
  modelSection.style.display = isAzureMai ? 'none' : '';
  localSection.style.display = mode === 'local' ? '' : 'none';
  azureEndpointInput.required = isAzureMai;

  apiLabel.textContent = isAzureMai ? 'Foundry API Key' : 'Groq API Key';
  apiInput.placeholder = isAzureMai ? 'Paste Foundry API key' : 'Paste Groq API key';

  const copy = ONBOARDING_COPY[mode] || ONBOARDING_COPY.webspeech;
  const deviceLabel = micSelect.value
    ? micSelect.options[micSelect.selectedIndex]?.textContent
    : 'Default system audio';

  const engineLabel = isAzureMai ? 'MAI transcription' : getModelLabel(mode, modelSelect.value);
  summary.textContent = `${deviceLabel} + ${engineLabel}`;
  explanationTitle.textContent = copy.title;
  explanationBody.textContent = copy.body;
  explanationSteps.innerHTML = '';
  copy.steps.forEach(step => {
    const li = document.createElement('li');
    li.textContent = step;
    explanationSteps.appendChild(li);
  });
}

async function loadOnboardingProviderSettings(mode) {
  if (mode === 'azure-mai') {
    apiInput.value = await invoke('load_azure_api_key').catch(() => null) || '';
    azureEndpointInput.value = await store.get('azureMaiEndpoint') || '';
    return;
  }

  if (mode === 'groq') {
    apiInput.value = await invoke('load_api_key').catch(() => null) || '';
    return;
  }

  apiInput.value = '';
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
  const azureEndpoint = azureEndpointInput.value.trim();

  if (mode === 'azure-mai' && !azureEndpoint) {
    azureEndpointInput.setCustomValidity('Enter your Azure AI Foundry project endpoint.');
    azureEndpointInput.reportValidity();
    return;
  }
  azureEndpointInput.setCustomValidity('');

  await store.set('audioDeviceId', deviceId);
  await store.set('transcriptionMode', mode);
  if (mode !== 'azure-mai') {
    await store.set(getModelKey(mode), model);
  }
  await store.set('localAcceleration', acceleration);
  await store.set('onboardingComplete', true);

  if (mode === 'groq' && apiInput.value.trim()) {
    await invoke('save_api_key', { key: apiInput.value.trim() });
  } else if (mode === 'azure-mai') {
    if (apiInput.value.trim()) {
      await invoke('save_azure_api_key', { key: apiInput.value.trim() });
    }
    await store.set('azureMaiEndpoint', azureEndpoint);
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
    radio.addEventListener('change', async () => {
      const mode = getSelectedMode();
      await loadOnboardingProviderSettings(mode);
      updateForMode(mode);
    });
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
  const savedKey = mode === 'azure-mai'
    ? await invoke('load_azure_api_key').catch(() => null)
    : await invoke('load_api_key').catch(() => null);
  const savedAzureEndpoint = await store.get('azureMaiEndpoint');

  if (savedAcceleration) {
    accelerationSelect.value = savedAcceleration;
  }
  if (savedKey) {
    apiInput.value = savedKey;
  }
  azureEndpointInput.value = savedAzureEndpoint || '';

  setSelectedMode(mode);
  populateModelSelect(mode, savedModel);
  updateForMode(mode);
  setupEvents();
}

document.addEventListener('DOMContentLoaded', init);
