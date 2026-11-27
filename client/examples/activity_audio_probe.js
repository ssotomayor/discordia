async function runProbe(options) {
  const sdk = window.LivekitClient || window.LiveKitClient;
  let room;
  window.LivekitClient = { ...sdk, Room: class extends sdk.Room {
    constructor(config) { super(config); room = this; this.on(sdk.RoomEvent.TrackSubscribed, track => { this.probeTrack = track; }); }
  }};
  window.__dxfActivityAudioWanted = options.request;
  window.__dxfActivityAudio.setVolume(0);
  let checked = false;
  const send = value => dioxus.send(value);
  await window.__dxfActivityAudio.start(options, value => {
    send(value);
    if (value.status !== 'playing' || checked) return;
    checked = true;
    void verify();
  });
  async function verify() {
    let context;
    let stage = 'element';
    try {
      const element = document.querySelector('audio');
      if (!element?.srcObject) throw new Error();
      stage = 'context';
      context = new AudioContext();
      await context.resume();
      stage = 'source';
      const tracks = room.probeTrack ? [room.probeTrack] : [];
      const media = tracks[0]?.mediaStreamTrack;
      const source = context.createMediaStreamSource(media ? new MediaStream([media]) : element.srcObject);
      const analyser = context.createAnalyser();
      const mute = context.createGain(); mute.gain.value = 0;
      source.connect(analyser); analyser.connect(mute); mute.connect(context.destination);
      const samples = new Float32Array(analyser.fftSize);
      stage = 'measure';
      const deadline = performance.now() + 10000;
      let pcm = false;
      while (performance.now() < deadline) {
        analyser.getFloatTimeDomainData(samples);
        if (samples.some(sample => Math.abs(sample) > 0.02)) { pcm = true; break; }
        await new Promise(resolve => setTimeout(resolve, 50));
      }
      source.disconnect(); analyser.disconnect(); mute.disconnect();
      await context.close(); context = null;
      stage = 'stats';
      let stats;
      if (tracks[0]?.receiver) (await tracks[0].receiver.getStats()).forEach(s => { if(s.type === 'inbound-rtp') stats = s; });
      const transport = tracks[0]?.receiver?.transport;
      const connection = {dtls: transport?.state, ice: transport?.iceTransport?.state};
      stage = 'stop';
      await window.__dxfActivityAudio.stop();
      send({ verified: pcm && !document.querySelector('audio'), finished: true, metrics: {...connection, track: !!tracks[0], receiver: !!tracks[0]?.receiver, packetsReceived: stats?.packetsReceived, totalAudioEnergy: stats?.totalAudioEnergy, totalSamplesReceived: stats?.totalSamplesReceived}, status: pcm ? 'verified' : 'No decoded PCM' });
    } catch (error) {
      if (context) await context.close();
      await window.__dxfActivityAudio.stop();
      send({ finished: true, metrics: {stage, error: error?.name}, status: 'WebView PCM verification failed' });
    }
  }
}
