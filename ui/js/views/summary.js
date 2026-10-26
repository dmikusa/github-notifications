// js/views/summary.js:
// Summary dashboard: mentions, CI failures, recent discussions, and review
// requests, stacked top to bottom. Mention and CI rows can be marked read,
// which clears them from the dashboard.
window.App = window.App || {};

window.App.views = window.App.views || {};

window.App.views.summary = (() => {
  async function markRead(btn) {
    const id = btn.dataset.threadId;
    if (!id || btn.disabled) return;
    btn.disabled = true;
    try {
      await window.App.api.postJSON('/api/threads/mark-read', { ids: [id] });
      window.App.flash.show('Marked read');
      await window.App.views.reload();
    } catch (err) {
      btn.disabled = false;
      window.App.flash.show(`Mark read failed: ${err.message}`, true);
    }
  }

  // Delegated so it keeps working after the view is swapped/reloaded.
  document.addEventListener('click', (e) => {
    const btn = e.target.closest('#view .summary-read');
    if (btn) markRead(btn);
  });

  return { markRead };
})();
