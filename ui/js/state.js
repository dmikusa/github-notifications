// js/state.js:
window.App = window.App || {};

window.App.state = {
  workspaces: [],
  currentWorkspace: '',
  currentView: 'queue',

  setWorkspaces(names) {
    this.workspaces = names.slice();
    if (!names.length) {
      this.currentWorkspace = '';
    } else if (!names.includes(this.currentWorkspace)) {
      this.currentWorkspace = names[0];
    }
  },
};