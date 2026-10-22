// For a resource's UI page: <script src="ivmp://ui/nui.js"></script>
//
//   ivmpNui.post('name', data)   calls RegisterNUICallback('name', function(data, cb) ... end) in the
//                                resource's client script; the promise gets what cb(result) passes.
//   window.addEventListener('message', (e) => e.data)   what the script sends with SendNUIMessage.
window.ivmpNui = (() => {
  let next = 0;
  const waiting = {};
  window.addEventListener('message', (event) => {
    const message = event.data;
    if (message && message.ivmpReply === true && waiting[message.id]) {
      waiting[message.id](message.result);
      delete waiting[message.id];
    }
  });
  return {
    post(name, data) {
      const id = ++next;
      return new Promise((resolve) => {
        waiting[id] = resolve;
        window.parent.postMessage({ ivmpCallback: true, name, data, id }, '*');
      });
    },
  };
})();
