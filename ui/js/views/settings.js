// js/views/settings.js:
// Settings sub-tabs: repo set management (remove sets/repos and browse an org
// to save a set) and discussion project management (add/remove projects and
// set the recent window). All edits write back to config.toml.
window.App = window.App || {};

window.App.settings = (() => {
  let org = '';
  let page = 1;
  let hasMore = false;
  let selected = new Set();
  let filterTimer = null;
  // Survive the view re-render after an edit so the user stays on the tab
  // they were working in.
  let activeTab = 'reposets';

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

  function showTab(tab) {
    activeTab = tab;
    const root = document.querySelector('#view');
    if (!root) return;
    root.querySelectorAll('[data-settings-tab]').forEach((btn) => {
      btn.classList.toggle('active', btn.dataset.settingsTab === tab);
    });
    root.querySelectorAll('[data-settings-panel]').forEach((panel) => {
      panel.hidden = panel.dataset.settingsPanel !== tab;
    });
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
    } catch (err) {
      // Don't leave the list stuck on "Loading repos…"; surface the failure.
      list.innerHTML = '';
      const p = document.createElement('p');
      p.className = 'empty error';
      p.textContent = `Couldn't load repos: ${err.message}`;
      list.appendChild(p);
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

    showTab(activeTab);
    root.querySelectorAll('[data-settings-tab]').forEach((btn) => {
      btn.addEventListener('click', () => showTab(btn.dataset.settingsTab));
    });

    root.querySelectorAll('[data-action="remove-set"]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        setBusy(btn, true, 'Removing\u2026');
        const ws = window.App.state.currentWorkspace;
        try {
          await window.App.api.deleteJSON(
            `/api/workspaces/${encodeURIComponent(ws)}/repo-sets/${encodeURIComponent(btn.dataset.set)}`
          );
          await reload();
        } catch (err) {
          window.App.flash.show(`Remove failed: ${err.message}`, true);
          setBusy(btn, false);
        }
      });
    });

    root.querySelectorAll('[data-action="remove-repo"]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        setBusy(btn, true, '\u2026');
        const ws = window.App.state.currentWorkspace;
        try {
          await window.App.api.deleteJSON(
            `/api/workspaces/${encodeURIComponent(ws)}/repo-sets/${encodeURIComponent(btn.dataset.set)}/repos/${encodeURIComponent(btn.dataset.repo)}`
          );
          await reload();
        } catch (err) {
          window.App.flash.show(`Remove failed: ${err.message}`, true);
          setBusy(btn, false);
        }
      });
    });

    root.querySelectorAll('[data-action="remove-discussion-project"]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        setBusy(btn, true, 'Removing\u2026');
        activeTab = 'discussions';
        try {
          await window.App.api.deleteJSON(
            `/api/discussions/projects/${encodeURIComponent(btn.dataset.project)}`
          );
          await reload();
        } catch (err) {
          window.App.flash.show(`Remove failed: ${err.message}`, true);
          setBusy(btn, false);
        }
      });
    });

    const projectForm = el('discussion-project-form');
    if (projectForm) {
      projectForm.addEventListener('submit', async (e) => {
        e.preventDefault();
        const project = projectForm.elements.project.value.trim();
        if (!project) return;
        const btn = projectForm.querySelector('button[type="submit"]');
        setBusy(btn, true, 'Adding\u2026');
        activeTab = 'discussions';
        try {
          await window.App.api.postJSON('/api/discussions/projects', { project });
          await reload();
        } catch (err) {
          window.App.flash.show(`Add project failed: ${err.message}`, true);
          setBusy(btn, false);
        }
      });
    }

    const windowForm = el('discussion-window-form');
    if (windowForm) {
      windowForm.addEventListener('submit', async (e) => {
        e.preventDefault();
        const days = Math.floor(Number(windowForm.elements.recent_days.value));
        if (!Number.isFinite(days) || days < 1) return;
        const btn = windowForm.querySelector('button[type="submit"]');
        setBusy(btn, true, 'Saving\u2026');
        activeTab = 'discussions';
        try {
          await window.App.api.postJSON('/api/discussions/settings', { recent_days: days });
          await reload();
        } catch (err) {
          window.App.flash.show(`Save failed: ${err.message}`, true);
          setBusy(btn, false);
        }
      });
    }

    const form = el('org-picker');
    if (!form) return;
    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      const setname = form.elements['set-name'].value.trim();
      // A trailing slash is a common typo and not part of the account name.
      org = form.elements.org.value.trim().replace(/\/+$/, '');
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
      } catch (err) {
        window.App.flash.show(`Save failed: ${err.message}`, true);
        setBusy(el('save-set'), false);
      }
    });
  }

  return { bind };
})();
