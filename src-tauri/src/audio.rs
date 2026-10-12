//! Enumerate native capture endpoints. WASAPI opens the selected ID directly.
use crate::AudioDevice;
use windows::Win32::{
    Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
    Foundation::RPC_E_CHANGED_MODE,
    Media::Audio::{eCapture, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE},
    System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    },
};

pub fn enumerate_capture_devices() -> Result<Vec<AudioDevice>, Box<dyn std::error::Error>> {
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED);
        if initialized.is_err() && initialized != RPC_E_CHANGED_MODE {
            initialized.ok()?;
        }
        struct Com(bool);
        impl Drop for Com {
            fn drop(&mut self) {
                if self.0 {
                    unsafe {
                        CoUninitialize();
                    }
                }
            }
        }
        let _com = Com(initialized.is_ok());
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let collection = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;
        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let device = collection.Item(index)?;
            let allocated_id = device.GetId()?;
            let id = allocated_id.to_string();
            CoTaskMemFree(Some(allocated_id.0.cast()));
            let id = id?;
            let properties = device.OpenPropertyStore(STGM_READ)?;
            // windows::PROPVARIANT clears its allocation on drop.
            let name = properties.GetValue(&PKEY_Device_FriendlyName)?.to_string();
            devices.push(AudioDevice { id, name });
        }
        Ok(devices)
    }
}
