// Drag to reorder the queue on the now-playing page: press a row's handle
// (.drag-handle) and move it; the rows it passes make room, and the page
// scrolls near its edges. Letting go calls window.ytmdlQueueMoved(from, to)
// with row positions, which the page sets (src/ui/mod.rs). Installed once;
// listens on the document, so it keeps working as the rows re-render.
(() => {
    if (window.ytmdlQueueDrag) return;
    window.ytmdlQueueDrag = true;

    // px from the scroller's top and bottom where dragging scrolls it.
    const EDGE = 72;
    let drag = null;

    document.addEventListener('pointerdown', e => {
        const handle = e.target.closest('.drag-handle');
        if (!handle || drag || e.button > 0) return;
        const row = handle.closest('li');
        const list = row && row.parentElement;
        const scroller = row && row.closest('.overlay');
        if (!list || !scroller) return;
        e.preventDefault();
        handle.setPointerCapture(e.pointerId);
        const rows = [...list.children];
        const top = scroller.scrollTop;
        drag = {
            pointer: e.pointerId,
            list,
            rows,
            scroller,
            from: rows.indexOf(row),
            to: rows.indexOf(row),
            // Row middles in the scroller's content coordinates.
            mids: rows.map(r => {
                const box = r.getBoundingClientRect();
                return box.top + box.height / 2 + top;
            }),
            height: row.getBoundingClientRect().height,
            startY: e.clientY,
            startTop: top,
            y: e.clientY,
            frame: 0,
        };
        list.classList.add('sorting');
        row.classList.add('dragging');
        drag.frame = requestAnimationFrame(step);
    });

    document.addEventListener('pointermove', e => {
        if (drag && e.pointerId === drag.pointer) drag.y = e.clientY;
    });

    function end(e) {
        if (!drag || e.pointerId !== drag.pointer) return;
        const d = drag;
        drag = null;
        cancelAnimationFrame(d.frame);
        d.y = e.clientY;
        place(d);
        // Without the transition, so the rows don't slide back from where
        // they were dropped.
        const reset = () => {
            d.list.classList.remove('sorting');
            for (const r of d.rows) {
                r.classList.remove('dragging');
                r.style.transform = '';
            }
        };
        if (e.type === 'pointercancel' || d.to === d.from) {
            reset();
            return;
        }
        // Hold the dropped order until the new one renders.
        let timer = 0;
        const observer = new MutationObserver(() => {
            observer.disconnect();
            clearTimeout(timer);
            reset();
        });
        observer.observe(d.list, { childList: true });
        timer = setTimeout(() => {
            observer.disconnect();
            reset();
        }, 1000);
        if (window.ytmdlQueueMoved) window.ytmdlQueueMoved(d.from, d.to);
    }
    document.addEventListener('pointerup', end);
    document.addEventListener('pointercancel', end);

    function step() {
        const d = drag;
        if (!d) return;
        const box = d.scroller.getBoundingClientRect();
        if (d.y < box.top + EDGE) d.scroller.scrollTop -= Math.ceil((box.top + EDGE - d.y) / 6);
        else if (d.y > box.bottom - EDGE) d.scroller.scrollTop += Math.ceil((d.y - box.bottom + EDGE) / 6);
        place(d);
        d.frame = requestAnimationFrame(step);
    }

    // Moves the dragged row under the pointer and the rows it passed out of
    // its way; sets where it would land.
    function place(d) {
        const dy = d.y - d.startY + d.scroller.scrollTop - d.startTop;
        const mid = d.mids[d.from] + dy;
        let to = d.from;
        while (to + 1 < d.mids.length && mid > d.mids[to + 1]) to++;
        while (to > 0 && mid < d.mids[to - 1]) to--;
        d.to = to;
        d.rows.forEach((r, i) => {
            let shift = '';
            if (i === d.from) shift = `translateY(${dy}px)`;
            else if (d.from < i && i <= to) shift = `translateY(${-d.height}px)`;
            else if (to <= i && i < d.from) shift = `translateY(${d.height}px)`;
            r.style.transform = shift;
        });
    }
})();
