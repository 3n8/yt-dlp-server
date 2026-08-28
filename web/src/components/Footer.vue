<script setup>
import { inject } from 'vue'
</script>
<script>
import { getAPIUrl } from '../utils';

export default {
  data: () => ({
    server_info: {},
    updating: false,
    percent: null,
    logTail: '',
    result: null,
  }),
  mounted() {
    this.setBookmarklet();
    this.server_info = inject('serverInfo');
    this.refreshServerInfo = inject('refreshServerInfo');
  },
  methods: {
    setBookmarklet() {
      let url = window.location.protocol + '//' + window.location.hostname
      if (window.location.port != '') {
        url = url + ':' + window.location.port;
      }
      if (window.location.protocol == 'https:') {
        document.getElementById('bookmarklet').href = "javascript:fetch(\"" + url
          + "/api/downloads\",{body:JSON.stringify({'url':window.location.href}),method:\"POST\",headers:{'Content-Type':'application/json'}});";
      }
      else {
        document.getElementById('bookmarklet').href = "javascript:(function(){document.body.innerHTML += '<form name=\"ydl_form\" method=\"POST\" action=\""
          + url
          + "/api/downloads\"><input name=\"url\" type=\"url\" value=\"'+window.location.href+'\"/></form>';document.ydl_form.submit()})();";
      }
    },
    async updateYtdlp() {
      if (this.updating) return;
      this.updating = true;
      this.percent = 0;
      this.logTail = '';
      this.result = null;
      try {
        const res = await fetch(getAPIUrl('api/yt-dlp/update', import.meta.env), { method: 'POST' });
        const data = await res.json();
        if (!data.success) {
          this.result = { success: false, message: data.error || 'Could not start update' };
          this.updating = false;
          return;
        }
        await this.watchUpdate(data.job_id);
      } catch (e) {
        this.result = { success: false, message: e.message || 'Network error' };
        this.updating = false;
      }
    },
    watchUpdate(jobId) {
      return new Promise((resolve) => {
        const src = new EventSource(getAPIUrl(`api/jobs/${jobId}/events`, import.meta.env));
        const finish = (payload) => {
          src.close();
          this.updating = false;
          this.percent = payload.success ? 100 : this.percent;
          this.result = payload;
          if (payload.success && this.refreshServerInfo) {
            this.refreshServerInfo();
          }
          resolve();
        };
        src.addEventListener('log', (ev) => {
          try {
            const data = JSON.parse(ev.data);
            this.logTail = (data.log || '').trim().split('\n').slice(-4).join('\n');
            if (typeof data.percent === 'number') this.percent = data.percent;
            else if (this.percent == null) this.percent = 10;
            else this.percent = Math.min(90, this.percent + 2);
          } catch { /* ignore */ }
        });
        src.addEventListener('done', (ev) => {
          try {
            finish(JSON.parse(ev.data));
          } catch {
            finish({ success: false, message: 'Update finished with an unknown result' });
          }
        });
        src.onerror = () => {
          if (this.updating) {
            finish({ success: false, message: 'Lost connection while updating' });
          }
        };
      });
    }
  }
}
</script>
<template>
  <footer class="footer text-center">
    <p class="text-muted">
      Drag and Drop the Bookmarklet to your bookmark bar for easy access: <a id="bookmarklet" class="badge badge-subtle"
        href="">{{ server_info.ydl_module_name }}</a>
      <br />
      Powered by
      <a target="_blank" rel="noopener noreferrer" class="footer-link" :href="server_info.ydl_module_website">{{
        server_info.ydl_module_name
      }}</a> version {{ server_info.ydl_module_version }}.
      Code &amp; issues on <a target="_blank" rel="noopener noreferrer" class="footer-link"
        href="https://github.com/3n8/yt-dlp-server">GitHub</a>.
      <span v-if="server_info.ydls_version != ''" data-toggle="tooltip" data-placement="top"
        :title="server_info.ydls_release_date">Rev <a target="_blank"
          :href="'https://github.com/3n8/yt-dlp-server/commit/' + server_info.ydls_version"
          class="badge badge-subtle">{{
            server_info.ydls_version }}</a>
      </span>
    </p>
    <div class="update-panel">
      <button class="btn btn-outline-secondary btn-sm" :disabled="updating" @click="updateYtdlp">
        <span v-if="updating" class="spinner-border spinner-border-sm me-1" role="status" aria-hidden="true"></span>
        {{ updating ? 'Updating yt-dlp…' : 'Update yt-dlp' }}
      </button>
      <span class="text-muted small ms-2">channel: {{ server_info.update_channel || 'nightly' }}</span>
      <div v-if="updating" class="update-progress">
        <div class="progress">
          <div class="progress-bar progress-bar-striped progress-bar-animated"
            :class="{ 'progress-bar-indeterminate': percent == null }"
            role="progressbar"
            :style="{ width: (percent == null ? 100 : percent) + '%' }"
            :aria-valuenow="percent || 0" aria-valuemin="0" aria-valuemax="100"></div>
        </div>
        <pre v-if="logTail" class="update-log">{{ logTail }}</pre>
      </div>
      <div v-if="result" class="update-result" :class="result.success ? 'update-ok' : 'update-fail'">
        {{ result.message }}
      </div>
    </div>
  </footer>
</template>
