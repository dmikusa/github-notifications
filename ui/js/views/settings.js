// js/views/settings.js:
// Repo set management: remove repo sets/repos, and an org browser that
// checkbox-selects repos and saves them into a repo set (config write-back).
// The org browser filters server-side (GitHub search) so it works across the
// whole org, not just the already-loaded page.
window.App = window.App || {};

window.App.settings = (() => {
  let org = '';
  let page = 1;
  let hasMore = false;
  let selected = new Set();
  let filterTimer = null;

  function el(id) {
    return document.getElementById(id);
  }

  async function reload() {
    await window.App.views.reload();
  }

  function setBusy(btn, busy, busyText) {
    if (!btn) return;
    if (busy) {
      btn.dataset.label = btn.dataset.label || btn.textContent;
      btn.disabled = true;
      btn.textContent = busyText || btn.dataset.label;
    } else {
      btn.disabled = false;
      btn.textContent = btn.dataset.label || btn.textContent;
    }
  }

  function renderRepos(repos) {
    const list = el('org-repo-list');
    for (const repo of repos) {
      const label = document.createElement('label');
      const cb = document.createElement('input');
      cb.type = 'checkbox';
      cb.className = 'org-repo';
      cb.value = repo.full_name;
      if (selected.has(repo.full_name)) cb.checked = true;
      cb.addEventListener('change', () => {
        if (cb.checked) {
          selected.add(repo.full_name);
        } else {
          selected.delete(repo.full_name);
        }
      });
      label.appendChild(cb);
      label.appendChild(document.createTextNode(` ${repo.full_name}`));
      list.appendChild(label);
    }
  }

  async function loadPage() {
    const q = (el('org-filter').value || '').trim();
    const params = new URLSearchParams({ page: String(page), q });
    const list = el('org-repo-list');
    list.innerHTML = '<div class="loading"><span class="spinner" aria-hidden="true"></span>Loading repos\u2026</div>';
    setBusy(el('load-repos'), true, 'Loading\u2026');
    setBusy(el('load-more'), true, 'Loading\u2026');
    try {
      const data = await window.App.api.getJSON(
        `/api/orgs/${encodeURIComponent(org)}/repos?${params}`
      );
      hasMore = data.has_more;
      list.innerHTML = '';
      renderRepos(data.repos);
      el('load-more').hidden = !hasMore;
      if (!data.repos.length) {
        list.innerHTML = '<p class="empty">No repos match.</p>';
      }
    } finally {
      setBusy(el('load-repos'), false);
      setBusy(el('load-more'), false);
    }
  }

  function onFilterInput() {
    clearTimeout(filterTimer);
    filterTimer = setTimeout(async () => {
      page = 1;
      await loadPage();
    }, 300);
  }

  function bind() {
    const root = document.querySelector('#view');
    if (!root) return;

    root.querySelectorAll('[data-action="remove-set"]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        setBusy(btn, true, 'Removing\u2026');
        const ws = window.App.state.currentWorkspace;
        await window.App.api.deleteJSON(
          `/api/workspaces/${encodeURIComponent(ws)}/repo-sets/${encodeURIComponent(btn.dataset.set)}`
        );
        reload();
      });
    });

    root.querySelectorAll('[data-action="remove-repo"]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        setBusy(btn, true, '\u2026');
        const ws = window.App.state.currentWorkspace;
        await window.App.api.deleteJSON(
          `/api/workspaces/${encodeURIComponent(ws)}/repo-sets/${encodeURIComponent(btn.dataset.set)}/repos/${encodeURIComponent(btn.dataset.repo)}`
        );
        reload();
      });
    });

    const form = el('org-picker');
    if (!form) return;
    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      const setname = form.elements['set-name'].value.trim();
      org = form.elements.org.value.trim();
      if (!setname || !org) return;
      el('org-repos').hidden = false;
      el('org-filter').value = '';
      selected = new Set();
      page = 1;
      hasMore = false;
      el('load-more').hidden = true;
      await loadPage();
    });

    el('load-more').addEventListener('click', async () => {
      page += 1;
      await loadPage();
    });

    el('org-filter').addEventListener('input', onFilterInput);

    el('save-set').addEventListener('click', async () => {
      const setname = form.elements['set-name'].value.trim();
      if (!setname) return;
      setBusy(el('save-set'), true, 'Saving\u2026');
      const ws = window.App.state.currentWorkspace;
      try {
        await window.App.api.postJSON(
          `/api/workspaces/${encodeURIComponent(ws)}/repo-sets`,
          { name: setname, repos: Array.from(selected) }
        );
        selected = new Set();
        await reload();
      } finally {
        setBusy(el('save-set'), false);
      }
    });
  }

  return { bind };
})();