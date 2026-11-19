// JavaScript side of the Elm ports declared in src/Ports.elm.
//
// - websocketOut / websocketIn: a single shared WebSocket to /v1/ws/builds.
//   Elm sends {type: "subscribe" | "unsubscribe", resource, id}; server
//   events are forwarded to Elm verbatim, plus synthetic
//   {type: "connected"} / {type: "disconnected"} status messages.
// - storeToken / clearToken / tokenReceived: JWT persistence in localStorage.
// - setTheme: "system" | "light" | "dark". Sets/clears data-theme on <html>
//   and persists the choice (index.html re-applies it before first paint).

const TOKEN_KEY = 'ekaci_token';
const THEME_KEY = 'ekaci_theme';
const MIN_BACKOFF_MS = 1000;
const MAX_BACKOFF_MS = 30000;

function readToken() {
    try {
        return window.localStorage.getItem(TOKEN_KEY);
    } catch (e) {
        console.warn('ekaci: unable to read token from localStorage', e);
        return null;
    }
}

function writeToken(token) {
    try {
        if (token === null) {
            window.localStorage.removeItem(TOKEN_KEY);
        } else {
            window.localStorage.setItem(TOKEN_KEY, token);
        }
    } catch (e) {
        console.warn('ekaci: unable to persist token to localStorage', e);
    }
}

// The saved theme override, or "system" when the OS preference applies.
export function readTheme() {
    try {
        const theme = window.localStorage.getItem(THEME_KEY);
        return theme === 'light' || theme === 'dark' ? theme : 'system';
    } catch (e) {
        return 'system';
    }
}

function applyTheme(theme) {
    const root = document.documentElement;
    if (theme === 'light' || theme === 'dark') {
        root.dataset.theme = theme;
    } else {
        delete root.dataset.theme;
    }
    try {
        if (theme === 'light' || theme === 'dark') {
            window.localStorage.setItem(THEME_KEY, theme);
        } else {
            window.localStorage.removeItem(THEME_KEY);
        }
    } catch (e) {
        console.warn('ekaci: unable to persist theme to localStorage', e);
    }
}

function websocketUrl(token) {
    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = new URL(`${proto}//${window.location.host}/v1/ws/builds`);
    if (token) {
        url.searchParams.set('token', token);
    }
    return url.toString();
}

function subscriptionKey(resource, id) {
    return `${resource}\u0000${id}`;
}

class BuildSocket {
    constructor(app) {
        this.app = app;
        this.socket = null;
        // Active subscriptions, replayed on every (re)connect.
        this.subscriptions = new Map();
        this.backoffMs = MIN_BACKOFF_MS;
        this.reconnectTimer = null;
    }

    toElm(message) {
        if (this.app.ports.websocketIn) {
            this.app.ports.websocketIn.send(message);
        }
    }

    connect() {
        if (this.socket) {
            return;
        }
        window.clearTimeout(this.reconnectTimer);
        this.reconnectTimer = null;

        const socket = new WebSocket(websocketUrl(readToken()));
        this.socket = socket;

        socket.addEventListener('open', () => {
            this.backoffMs = MIN_BACKOFF_MS;
            for (const sub of this.subscriptions.values()) {
                this.send({ type: 'subscribe', ...sub });
            }
            this.toElm({ type: 'connected' });
        });

        socket.addEventListener('message', (event) => {
            try {
                this.toElm(JSON.parse(event.data));
            } catch (e) {
                console.warn('ekaci: ignoring malformed websocket message', e);
            }
        });

        socket.addEventListener('close', () => {
            if (this.socket !== socket) {
                return; // Replaced by a deliberate reconnect.
            }
            this.socket = null;
            this.toElm({ type: 'disconnected' });
            this.scheduleReconnect();
        });
        // 'error' is always followed by 'close', which handles recovery.
    }

    scheduleReconnect() {
        if (this.reconnectTimer !== null) {
            return;
        }
        const delay = this.backoffMs;
        this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
        this.reconnectTimer = window.setTimeout(() => {
            this.reconnectTimer = null;
            this.connect();
        }, delay);
    }

    // Reconnect immediately, e.g. after the auth token changes.
    reconnect() {
        const old = this.socket;
        this.socket = null;
        if (old) {
            old.close();
        }
        this.backoffMs = MIN_BACKOFF_MS;
        this.connect();
    }

    send(message) {
        if (this.socket && this.socket.readyState === WebSocket.OPEN) {
            this.socket.send(JSON.stringify(message));
        }
        // Otherwise the subscription set is replayed on 'open'.
    }

    handleFromElm(message) {
        if (!message || typeof message.type !== 'string') {
            return;
        }
        const key = subscriptionKey(message.resource, message.id);
        switch (message.type) {
            case 'connect':
                this.connect();
                break;
            case 'subscribe':
                this.subscriptions.set(key, { resource: message.resource, id: message.id });
                this.connect();
                this.send(message);
                break;
            case 'unsubscribe':
                this.subscriptions.delete(key);
                this.send(message);
                break;
            default:
                console.warn('ekaci: unknown outgoing websocket message', message);
        }
    }
}

export function setupPorts(app) {
    const ports = app.ports || {};
    const socket = new BuildSocket(app);

    if (ports.websocketOut) {
        ports.websocketOut.subscribe((message) => socket.handleFromElm(message));
    }

    if (ports.storeToken) {
        ports.storeToken.subscribe((token) => {
            writeToken(token);
            socket.reconnect();
        });
    }

    if (ports.clearToken) {
        ports.clearToken.subscribe(() => {
            writeToken(null);
            socket.reconnect();
        });
    }

    if (ports.setTheme) {
        ports.setTheme.subscribe(applyTheme);
    }

    // Defer so Elm's subscriptions are registered before we send.
    window.setTimeout(() => {
        if (ports.tokenReceived) {
            ports.tokenReceived.send(readToken());
        }
        socket.connect();
    }, 0);
}
