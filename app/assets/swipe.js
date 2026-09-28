// Swipe a song row sideways to act on it: in the queue to remove it
// (either way), on a playlist to play it next (to the right). The rows of a
// list with data-swipe opt in. The row follows the finger over the action's
// colour and name; let go past about a third of its width (or flick it) and the
// row's hidden .swipe-action button is clicked, whose handler (src/ui) does
// the action. Row state lives in data- attributes, which Dioxus doesn't
// touch when it re-renders a row. Installed once; listens on the document,
// so it keeps working as the rows re-render.
(() => {
    if (window.ytmdlSwipe) return;
    window.ytmdlSwipe = true;

    // px a finger moves before it counts as a swipe (or a scroll).
    const SLOP = 10;
    // Share of the row's width past which letting go acts.
    const COMMIT = 0.35;
    // px/ms of a flick that acts from a shorter swipe, measured over the
    // last SPEED_MS of the movement.
    const FLICK = 0.5;
    const SPEED_MS = 100;
    const MS = 180;

    let swipe = null;
    // A swipe that let go over a row doesn't also tap it.
    let swallowClickUntil = 0;

    // Which ways a list's rows go: 1 to the right, -1 to the left.
    const ways = kind => (kind === 'next' ? [1] : [1, -1]);

    document.addEventListener('pointerdown', e => {
        if (e.button > 0 || !e.isPrimary) return;
        // A new first finger ends a swipe whose end never came.
        if (swipe) {
            const old = swipe;
            swipe = null;
            if (old.active) settle(old, 0, () => clear(old.row));
        }
        const row = e.target.closest('[data-swipe] > li');
        // The queue's drag handle reorders instead. A swipe may start on the
        // ⋮ button, where a thumb swiping left starts; a tap there still
        // opens the menu.
        if (!row || e.target.closest('.drag-handle') || row.dataset.swiping) return;
        swipe = {
            pointer: e.pointerId,
            row,
            kind: row.parentElement.dataset.swipe,
            x0: e.clientX,
            y0: e.clientY,
            dx: 0,
            active: false,
            // Recent positions, for the flick speed.
            recent: [{ x: e.clientX, t: e.timeStamp }],
        };
    });

    document.addEventListener('pointermove', e => {
        const s = swipe;
        if (!s || e.pointerId !== s.pointer) return;
        const dx = e.clientX - s.x0;
        const dy = e.clientY - s.y0;
        if (!s.active) {
            if (Math.abs(dy) > SLOP && Math.abs(dy) >= Math.abs(dx)) {
                swipe = null; // a scroll
                return;
            }
            if (Math.abs(dx) <= SLOP || Math.abs(dx) < Math.abs(dy) * 1.2) return;
            if (!ways(s.kind).includes(Math.sign(dx))) {
                swipe = null;
                return;
            }
            s.active = true;
            s.width = s.row.offsetWidth;
            s.row.dataset.swiping = '';
            try {
                s.row.setPointerCapture(e.pointerId);
            } catch (_) {}
        }
        e.preventDefault();
        // Only the ways the list allows: a playlist row dragged back left stops in place.
        s.dx = ways(s.kind).includes(Math.sign(dx)) ? dx : 0;
        s.recent.push({ x: e.clientX, t: e.timeStamp });
        while (s.recent.length > 2 && e.timeStamp - s.recent[0].t > SPEED_MS) s.recent.shift();
        show(s);
    });

    function end(e) {
        const s = swipe;
        if (!s || e.pointerId !== s.pointer) return;
        swipe = null;
        if (!s.active) return;
        swallowClickUntil = performance.now() + 400;
        const a = s.recent[0];
        const b = s.recent[s.recent.length - 1];
        const speed = b.t > a.t ? (b.x - a.x) / (b.t - a.t) : 0;
        const flicked = Math.abs(speed) >= FLICK && Math.sign(speed) === Math.sign(s.dx) && Math.abs(s.dx) > 40;
        const act = e.type !== 'pointercancel' && s.dx !== 0 && (armed(s) || flicked);
        if (!act) return settle(s, 0, () => clear(s.row));
        if (s.kind === 'remove') {
            // Off the side, fold away, then out of the queue.
            settle(s, Math.sign(s.dx) * s.width, () => {
                fold(s.row);
                click(s.row);
                // Put it back if it's still here after a while (removal failed).
                setTimeout(() => s.row.isConnected && clear(s.row, true), 3000);
            });
        } else {
            click(s.row);
            settle(s, 0, () => clear(s.row));
        }
    }
    document.addEventListener('pointerup', end);
    document.addEventListener('pointercancel', end);

    // Nor does dragging a row's cover image (with a mouse) start an image drag.
    document.addEventListener('dragstart', e => {
        if (e.target instanceof Element && e.target.closest('[data-swipe] > li')) e.preventDefault();
    });

    document.addEventListener(
        'click',
        e => {
            if (e.isTrusted && performance.now() < swallowClickUntil) {
                e.stopPropagation();
                e.preventDefault();
            }
        },
        true,
    );

    const armed = s => Math.abs(s.dx) >= s.width * COMMIT;

    function show(s) {
        const row = s.row;
        row.style.setProperty('--swipe', `${s.dx}px`);
        row.dataset.swiping = s.dx < 0 ? 'left' : 'right';
        if (armed(s)) row.dataset.armed = '';
        else delete row.dataset.armed;
    }

    // Slides the row to `x` px, then calls `done`.
    function settle(s, x, done) {
        const row = s.row;
        row.dataset.settling = '';
        row.style.setProperty('--swipe', `${x}px`);
        setTimeout(done, MS);
    }

    function fold(row) {
        row.style.height = `${row.offsetHeight}px`;
        row.offsetHeight; // start the fold from the row's height
        row.dataset.folding = '';
        row.style.height = '0px';
    }

    function clear(row, all) {
        for (const key of ['swiping', 'armed', 'settling', 'folding']) delete row.dataset[key];
        row.style.removeProperty('--swipe');
        if (all) row.style.removeProperty('height');
    }

    function click(row) {
        const button = row.querySelector(':scope > .swipe-action');
        if (button) button.click();
    }
})();
