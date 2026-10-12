// Press and hold a song row or tile to open its menu: the row's ⋮ button
// (a direct child with class .more, which tiles have hidden) is clicked,
// whose handler (src/ui) opens the menu. Moving the finger first (a scroll or
// a swipe) cancels it, and letting go after it doesn't also tap the row. While
// the finger stays down the row is marked data-held, so assets/swipe.js
// doesn't start a swipe under the open menu. Installed once; listens on the
// document, so it keeps working as the rows re-render.
(() => {
    if (window.ytmdlLongPress) return;
    window.ytmdlLongPress = true;

    // ms the finger stays down, and px it may move meanwhile.
    const HOLD = 450;
    const SLOP = 10;

    let press = null;
    // Letting go of a long press doesn't also tap the row.
    let swallowClickUntil = 0;

    const target = el => {
        const row = el instanceof Element && el.closest('li');
        return row && row.querySelector(':scope > .more') ? row : null;
    };

    document.addEventListener('pointerdown', e => {
        cancel();
        if (e.button > 0 || !e.isPrimary) return;
        const row = target(e.target);
        // Buttons in the row (⋮, download, the queue's drag handle) do their own thing.
        if (!row || e.target.closest('button')) return;
        press = {
            pointer: e.pointerId,
            row,
            x0: e.clientX,
            y0: e.clientY,
            fired: false,
            timer: setTimeout(() => fire(), HOLD),
        };
    });

    function fire() {
        const p = press;
        if (!p || !p.row.isConnected) return cancel();
        p.fired = true;
        p.row.dataset.held = '';
        const button = p.row.querySelector(':scope > .more');
        if (button) button.click();
    }

    document.addEventListener('pointermove', e => {
        const p = press;
        if (!p || e.pointerId !== p.pointer || p.fired) return;
        if (Math.abs(e.clientX - p.x0) > SLOP || Math.abs(e.clientY - p.y0) > SLOP) cancel();
    });

    function end(e) {
        const p = press;
        if (!p || e.pointerId !== p.pointer) return;
        if (p.fired) swallowClickUntil = performance.now() + 400;
        cancel();
    }
    document.addEventListener('pointerup', end);
    document.addEventListener('pointercancel', end);
    // A scroll that started without moving the pointer much (momentum, say).
    document.addEventListener('scroll', () => press && !press.fired && cancel(), true);

    function cancel() {
        const p = press;
        press = null;
        if (!p) return;
        clearTimeout(p.timer);
        delete p.row.dataset.held;
    }

    // The WebView's own long-press handling (a context menu, selection).
    document.addEventListener('contextmenu', e => {
        if (target(e.target)) e.preventDefault();
    });

    document.addEventListener(
        'click',
        e => {
            if (e.isTrusted && (press?.fired || performance.now() < swallowClickUntil)) {
                e.stopPropagation();
                e.preventDefault();
            }
        },
        true,
    );
})();
