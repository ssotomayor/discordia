const back = () => {
  dioxus.send(null);
  return true;
};
window.__dxfAndroidBack = back;
try {
  await new Promise(resolve => window.addEventListener('pagehide', resolve, { once: true }));
} finally {
  if (window.__dxfAndroidBack === back) delete window.__dxfAndroidBack;
}
