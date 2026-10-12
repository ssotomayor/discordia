(() => {
  if (window.__dxfGifPlayback) return;
  window.__dxfGifPlayback = true;
  const images = new Map();
  let focused = document.hasFocus();
  const isGif = src => /^data:image\/gif[;,]/i.test(src) || /\.gif(?:[?#]|$)/i.test(src);

  function update(img, state) {
    const play = state.visible && focused && !document.hidden &&
      (!img.hasAttribute('data-gif-hover') || state.hovered);
    if (!play && !state.poster && !state.posterFailed && img.getAttribute('src') === state.source &&
        img.complete && img.naturalWidth && img.naturalHeight) {
      try {
        const canvas = document.createElement('canvas');
        canvas.width = img.naturalWidth;
        canvas.height = img.naturalHeight;
        canvas.getContext('2d').drawImage(img, 0, 0);
        state.poster = canvas.toDataURL('image/png');
        canvas.width = canvas.height = 0;
      } catch {
        // Remote images without CORS cannot supply a readable poster frame.
        state.posterFailed = true;
      }
    }
    const src = play ? state.source : state.poster;
    if (src && img.getAttribute('src') !== src) img.setAttribute('src', src);
  }

  const observer = new IntersectionObserver(entries => {
    for (const entry of entries) {
      const state = images.get(entry.target);
      if (!state) continue;
      state.visible = entry.isIntersecting && entry.intersectionRatio > 0;
      update(entry.target, state);
    }
  });

  function track(img) {
    const src = img.getAttribute('src') || '';
    const old = images.get(img);
    if (old && (src === old.source || src === old.poster)) {
      update(img, old);
      return;
    }
    if (old) untrack(img, old);
    if (!isGif(src)) return;
    const hoverTarget = img.hasAttribute('data-gif-hover') ? img.closest('button') || img : img;
    const state = { source: src, poster: null, visible: false, hovered: hoverTarget.matches(':hover'), hoverTarget };
    state.load = () => update(img, state);
    state.enter = () => { state.hovered = true; update(img, state); };
    state.leave = () => { state.hovered = false; update(img, state); };
    img.addEventListener('load', state.load);
    hoverTarget.addEventListener('mouseenter', state.enter);
    hoverTarget.addEventListener('mouseleave', state.leave);
    images.set(img, state);
    observer.observe(img);
    update(img, state);
  }

  function untrack(img, state) {
    observer.unobserve(img);
    img.removeEventListener('load', state.load);
    state.hoverTarget.removeEventListener('mouseenter', state.enter);
    state.hoverTarget.removeEventListener('mouseleave', state.leave);
    images.delete(img);
  }

  function scan(root) {
    if (root.matches?.('img')) track(root);
    root.querySelectorAll?.('img').forEach(track);
  }

  new MutationObserver(records => {
    for (const record of records) {
      if (record.type === 'attributes') track(record.target);
      else record.addedNodes.forEach(scan);
    }
    for (const [img, state] of images) if (!img.isConnected) untrack(img, state);
  }).observe(document.documentElement, {
    subtree: true, childList: true, attributes: true, attributeFilter: ['src', 'data-gif-hover'],
  });
  function refresh() {
    for (const [img, state] of images) {
      state.hovered = state.hoverTarget.matches(':hover');
      update(img, state);
    }
  }
  window.addEventListener('blur', () => { focused = false; refresh(); });
  window.addEventListener('focus', () => { focused = true; refresh(); });
  document.addEventListener('visibilitychange', refresh);
  scan(document);
})();
