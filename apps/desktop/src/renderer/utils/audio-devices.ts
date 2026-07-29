let audioDevicesLogged = false;

export const findCaptureAudioDevice = async (): Promise<MediaDeviceInfo | null> => {
  const devices = await navigator.mediaDevices.enumerateDevices();

  if (!audioDevicesLogged) {
    audioDevicesLogged = true;
    const allInputs = devices.filter((d) => d.kind === 'audioinput');
    console.log(
      '[findCaptureAudioDevice] all audioinput devices:',
      allInputs.map((d) => `${d.deviceId.substring(0, 8)}… "${d.label}" group=${d.groupId.substring(0, 8)}…`),
    );
  }

  const target = devices.find((d) => d.kind === 'audioinput' && d.label.toLowerCase().includes('slopcast'));
  if (!target) return null;

  console.log(
    `[findCaptureAudioDevice] found: id=${target.deviceId.substring(0, 8)}… label="${target.label}" group=${target.groupId.substring(0, 8)}…`,
  );
  return target;
};
