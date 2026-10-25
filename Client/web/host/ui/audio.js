// Positional audio for resources (docs/audio.md). The game sends audio.play / audio.coords /
// audio.stop / audio.volume / audio.listener / audio.occlusion / audio.master, and the sounds play
// here with WebAudio: a 3D panner per sound (distance falloff, left/right from where the listener
// looks), and an occlusion stage (a low-pass filter and less volume) the game drives from what is
// between the listener and the sound (interiors, walls), like the game's own sounds are muffled.
// Chromium decodes mp3, wav, ogg and flac; the files are the resource's own (ivmp://<resource>/...).
(() => {
  let context = null;
  let master = null;
  const buffers = new Map();
  const sounds = new Map();

  function ensure() {
    if (!context) {
      context = new (window.AudioContext || window.webkitAudioContext)({ latencyHint: 'interactive' });
      master = context.createGain();
      master.connect(context.destination);
    }
    if (context.state === 'suspended') context.resume();
    return context;
  }

  // The decoded file, kept: the same sound is often played again.
  function load(url) {
    if (!buffers.has(url)) {
      buffers.set(
        url,
        fetch(url)
          .then((response) => {
            if (!response.ok) throw new Error(`${url}: ${response.status}`);
            return response.arrayBuffer();
          })
          .then((bytes) => ensure().decodeAudioData(bytes)),
      );
      buffers.get(url).catch(() => buffers.delete(url));
    }
    return buffers.get(url);
  }

  const smooth = (param, value, seconds = 0.05) => param.setTargetAtTime(value, context.currentTime, seconds);

  ivmp.on('audio.play', async ({ id, url, x, y, z, volume = 1, radius = 30, loop = false, frontend = false }) => {
    ensure();
    let buffer;
    try {
      buffer = await load(url);
    } catch (e) {
      console.error(`audio: cannot play ${url}: ${e.message}`);
      return;
    }
    if (sounds.has(id) && sounds.get(id).cancelled) {
      sounds.delete(id);
      return;
    }
    const source = context.createBufferSource();
    source.buffer = buffer;
    source.loop = loop;
    const lowpass = context.createBiquadFilter();
    lowpass.type = 'lowpass';
    lowpass.frequency.value = 22000;
    const occlusion = context.createGain();
    const level = context.createGain();
    level.gain.value = volume;
    source.connect(lowpass).connect(occlusion).connect(level);
    let panner = null;
    if (frontend) {
      level.connect(master);
    } else {
      panner = context.createPanner();
      panner.panningModel = 'HRTF';
      panner.distanceModel = 'linear';
      panner.refDistance = Math.min(2, radius * 0.1);
      panner.maxDistance = radius;
      panner.rolloffFactor = 1;
      panner.positionX.value = -x;
      panner.positionY.value = y;
      panner.positionZ.value = z;
      level.connect(panner).connect(master);
    }
    const sound = { source, lowpass, occlusion, level, panner };
    sounds.set(id, sound);
    source.onended = () => {
      if (sounds.get(id) === sound) sounds.delete(id);
      source.disconnect();
    };
    source.start();
  });

  ivmp.on('audio.stop', ({ id }) => {
    const sound = sounds.get(id);
    if (sound) {
      try { sound.source.stop(); } catch (e) { /* already over */ }
      sounds.delete(id);
    } else {
      // Asked to stop before the file had loaded.
      sounds.set(id, { cancelled: true });
    }
  });

  ivmp.on('audio.coords', ({ id, x, y, z }) => {
    const sound = sounds.get(id);
    if (!sound || !sound.panner) return;
    smooth(sound.panner.positionX, -x, 0.03);
    smooth(sound.panner.positionY, y, 0.03);
    smooth(sound.panner.positionZ, z, 0.03);
  });

  ivmp.on('audio.volume', ({ id, volume }) => {
    const sound = sounds.get(id);
    if (sound && sound.level) smooth(sound.level.gain, volume);
  });

  // 0 = clear, 1 = fully blocked: quieter and muffled (22 kHz down to about 440 Hz).
  ivmp.on('audio.occlusion', ({ id, o }) => {
    const sound = sounds.get(id);
    if (!sound || !sound.lowpass) return;
    smooth(sound.occlusion.gain, 1 - 0.97 * o, 0.1);
    smooth(sound.lowpass.frequency, 22000 * Math.pow(0.01, o), 0.1);
  });

  ivmp.on('audio.master', ({ volume }) => {
    ensure();
    smooth(master.gain, volume);
  });

  // The x axis is mirrored (-x everywhere): the game's left/right came out swapped in the headphones.
  // Where the listener is and which way it looks (the game camera): x east, y north, z up.
  ivmp.on('audio.listener', ({ p, f, u }) => {
    if (!context) return;
    const l = context.listener;
    l.positionX.value = -p[0];
    l.positionY.value = p[1];
    l.positionZ.value = p[2];
    l.forwardX.value = -f[0];
    l.forwardY.value = f[1];
    l.forwardZ.value = f[2];
    l.upX.value = u[0];
    l.upY.value = u[1];
    l.upZ.value = u[2];
  });
})();
