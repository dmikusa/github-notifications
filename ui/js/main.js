// js/main.js:
window.App = window.App || {};

(async () => {
  const main = document.getElementById('main');

  try {
    const state = await window.App.state.refresh();
    const auth = state.auth;
    let authText;
    if (!auth.authenticated) {
      authText = `${auth.provider} (no token)`;
    } else if (auth.ok) {
      authText = `${auth.provider} as ${auth.login || 'unknown'}`;
    } else {
      authText = `${auth.provider} as ${auth.login || 'unknown'} \u2014 missing scopes: ${auth.missing.join(', ') || 'unknown'}`;
    }
    main.textContent =
      `Connected. v${state.version} \u00b7 ${state.workspaces.length} workspace(s) ` +
      `\u00b7 auth: ${authText}` +
      (state.sync.last_sync ? ` \u00b7 last sync ${state.sync.last_sync}` : '');
  } catch (err) {
    main.textContent = `Failed to reach the daemon: ${err.message}`;
  }
})();