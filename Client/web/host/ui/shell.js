// The page that is always there: it shows the resources' UIs (one frame each, ivmp://<resource>/<page>)
// and carries messages between them and the game. A resource page uses nui.js; this file is the
// other half: the game sends ui.open / ui.close / ui.message / ui.reply, a frame's
// ivmpNui.post(name, data) goes to the game as ui.callback.
(() => {
  const container = document.getElementById('frames');
  const frames = {};

  function frameOf(win) {
    return Object.keys(frames).find((resource) => frames[resource].contentWindow === win);
  }

  ivmp.on('ui.open', ({ resource, page }) => {
    if (frames[resource]) frames[resource].remove();
    const frame = document.createElement('iframe');
    frame.src = `ivmp://${resource}/${page}`;
    frames[resource] = frame;
    container.appendChild(frame);
  });

  ivmp.on('ui.close', ({ resource }) => {
    if (frames[resource]) frames[resource].remove();
    delete frames[resource];
  });

  // SetNuiFocus: the keyboard goes into the resource's frame (a frame can't take it by itself).
  ivmp.on('ui.focus', ({ resource, focus }) => {
    const frame = frames[resource];
    if (!frame) return;
    if (focus) frame.focus();
    else if (document.activeElement === frame) frame.blur();
  });

  // SendNUIMessage: the page gets the table as a plain message event.
  ivmp.on('ui.message', ({ resource, data }) => {
    const frame = frames[resource];
    if (frame && frame.contentWindow) frame.contentWindow.postMessage(data, '*');
  });

  // The answer to a callback (cb(result) in RegisterNUICallback).
  ivmp.on('ui.reply', ({ resource, id, result }) => {
    const frame = frames[resource];
    if (frame && frame.contentWindow) frame.contentWindow.postMessage({ ivmpReply: true, id, result }, '*');
  });

  window.addEventListener('message', (event) => {
    const message = event.data;
    if (!message || message.ivmpCallback !== true) return;
    const resource = frameOf(event.source);
    if (!resource) return;
    ivmp.send('ui.callback', { resource, name: String(message.name), data: message.data, id: message.id });
  });
})();
