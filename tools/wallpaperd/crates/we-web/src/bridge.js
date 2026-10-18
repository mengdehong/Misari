(() => {
    const host = window.__wallpaperdHost;
    delete window.__wallpaperdHost;
    const listeners = new Map();
    let properties = {};
    let directories = {};
    let general = {};
    let media = {};
    let paused;
    const call = (fn, ...args) => {
        try { if (typeof fn === 'function') fn(...args); }
        catch (error) { console.error(error); }
    };
    const emit = (name, value) => call(listeners.get(name), value);
    window.wallpaperRequestRandomFileForProperty = (name, callback) => {
        const files = directories[name]?.files || [];
        call(callback, name, files.length ? files[Math.floor(Math.random() * files.length)] : '');
    };
    window.wallpaperRegisterAudioListener = fn => {
        listeners.set('audio', fn);
        host('audio');
    };
    const mediaEvents = {
        Status: value => ({enabled: !!value.enabled}),
        Properties: value => ({title: value.title || '', artist: value.artist || '',
            subTitle: '', albumTitle: value.album_title || '', albumArtist: value.album_artist || '',
            genres: value.genres || '', contentType: value.content_type || ''}),
        Playback: value => ({state: {playing: 1, paused: 2, stopped: 0}[value.playback] || 0}),
        Timeline: value => ({position: value.position || 0, duration: value.duration || 0}),
        // Registration is available even though this backend does not deliver cover art.
        Thumbnail: () => ({thumbnail: '', primaryColor: '#000000', secondaryColor: '#ffffff',
            tertiaryColor: '#ffffff', textColor: '#ffffff', highContrastColor: '#ffffff'}),
    };
    window.wallpaperMediaIntegration = {PLAYBACK_STOPPED: 0, PLAYBACK_PLAYING: 1, PLAYBACK_PAUSED: 2};
    for (const [name, event] of Object.entries(mediaEvents)) {
        window[`wallpaperRegisterMedia${name}Listener`] = fn => {
            listeners.set(name, fn);
            call(fn, event(media));
        };
    }
    let initialized = false;
    let frameRevision = -1;
    document.addEventListener('visibilitychange', () => {
        const revision = frameRevision;
        if (document.hidden) {
            host('hidden', revision);
        } else {
            requestAnimationFrame(() => requestAnimationFrame(() =>
                setTimeout(() => host('visible', revision), 0)));
        }
    });
    window.__wallpaperdReceive = state => {
        const listener = window.wallpaperPropertyListener || {};
        const previousDirectories = directories;
        if (state.directories) directories = state.directories;
        if (state.properties) {
            const changed = {};
            for (const [key, value] of Object.entries(state.properties)) {
                if (!initialized || JSON.stringify(value) !== JSON.stringify(properties[key]))
                    changed[key] = {value};
            }
            properties = state.properties;
            if (Object.keys(changed).length) call(listener.applyUserProperties?.bind(listener), changed);
        }
        if (state.directories) {
            for (const [name, directory] of Object.entries(directories)) {
                if (directory.mode !== 'fetchall') continue;
                const old = previousDirectories[name]?.files || [];
                const files = directory.files || [];
                const removed = old.filter(file => !files.includes(file));
                const added = files.filter(file => !old.includes(file));
                if (removed.length) call(listener.userDirectoryFilesRemoved?.bind(listener), name, removed);
                if (added.length) call(listener.userDirectoryFilesAddedOrChanged?.bind(listener), name, added);
            }
        }
        if (state.playback) {
            const next = {fps: state.playback.fps};
            if (!initialized || next.fps !== general.fps) {
                general = next;
                call(listener.applyGeneralProperties?.bind(listener), next);
            }
            if (!initialized || paused !== state.playback.paused) {
                paused = state.playback.paused;
                call(listener.setPaused?.bind(listener), paused);
            }
        }
        if (state.audio) emit('audio', state.audio);
        if (state.media && JSON.stringify(state.media) !== JSON.stringify(media)) {
            const previous = media;
            media = state.media;
            for (const [name, event] of Object.entries(mediaEvents)) {
                const next = event(media);
                if (JSON.stringify(next) !== JSON.stringify(event(previous))) emit(name, next);
            }
        }
        initialized = true;
        // Apply author properties before the first published browser frame.
        if (Object.hasOwn(state, 'frame_revision') && frameRevision !== state.frame_revision) {
            frameRevision = state.frame_revision;
            const revision = frameRevision;
            // Leave the second animation callback before acknowledging the
            // revision. The browser completes that rendering update, including
            // paint, before running the queued task.
            requestAnimationFrame(() => requestAnimationFrame(() =>
                setTimeout(() => host('ready', revision), 0)));
        }
    };
})();
