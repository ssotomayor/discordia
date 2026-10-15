(() => {
  const main = document.getElementById('main');
  const startup = document.getElementById('startup');
  let revealing = false;
  const observer = new MutationObserver(reveal);

  function reveal() {
    if (revealing || !main.querySelector('.dxf-ui')) return;
    revealing = true;
    observer.disconnect();
    // The first DOM batch can arrive before font loading has finished.
    Promise.resolve(document.fonts?.ready).catch(() => {}).then(() => {
      requestAnimationFrame(() => {
        main.setAttribute('data-ready', '');
        main.removeAttribute('inert');
        main.removeAttribute('aria-hidden');
        startup.remove();
      });
    });
  }

  observer.observe(main, { childList: true, subtree: true });
  reveal();
})();
