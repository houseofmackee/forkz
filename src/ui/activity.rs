//! Running jobs: each runs on its own thread and shows a row (progress,
//! details, and a Stop button) at the bottom of the window until it ends.
//! Transfers beyond the simultaneous limit wait in a queue, which can be
//! reordered from a row's menu; a header pauses or stops them all.

use std::cell::{OnceCell, RefCell};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBezelStyle, NSButton, NSMenu, NSMenuDelegate, NSMenuItem, NSProgressIndicator, NSProgressIndicatorStyle,
    NSStackView, NSTextField, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSEdgeInsets, NSObject, NSObjectProtocol};

use super::dialogs;
use super::util::{label, ns, small_label, symbol};
use crate::fs::format_size;
use crate::fs::ops::{self, Answer, CancelToken, Conflict, ErrorAnswer, Job, JobKind, Observer, Outcome, Progress};

struct Row {
    id: isize,
    view: Retained<NSStackView>,
    detail: Retained<NSTextField>,
    bar: Retained<NSProgressIndicator>,
    cancel: CancelToken,
    /// Pause and Resume (jobs only).
    pause: Option<Retained<NSButton>>,
    /// Start Next (shown while the job waits its turn).
    front: Option<Retained<NSButton>>,
}

/// A small borderless button with an SF Symbol.
fn icon_button(
    symbol_name: &str,
    tip: &str,
    id: isize,
    target: &Activity,
    action: objc2::runtime::Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    let button = NSButton::new(mtm);
    button.setBezelStyle(NSBezelStyle::Circular);
    button.setBordered(false);
    if let Some(image) = symbol(symbol_name, tip) {
        button.setImage(Some(&image));
    } else {
        button.setTitle(&ns(tip));
    }
    button.setToolTip(Some(&ns(tip)));
    button.setTag(id);
    unsafe {
        button.setTarget(Some(target));
        button.setAction(Some(action));
    }
    button
}

/// What a row's menu items do (encoded in their tags with the row's id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueAction {
    StartNow = 1,
    ToTop,
    Up,
    Down,
    ToBottom,
    Pause,
    Stop,
    PauseAll,
    ResumeAll,
    StopAll,
}

impl QueueAction {
    const ALL: [QueueAction; 10] = [
        QueueAction::StartNow,
        QueueAction::ToTop,
        QueueAction::Up,
        QueueAction::Down,
        QueueAction::ToBottom,
        QueueAction::Pause,
        QueueAction::Stop,
        QueueAction::PauseAll,
        QueueAction::ResumeAll,
        QueueAction::StopAll,
    ];

    fn tag(self, id: isize) -> isize {
        id * 16 + self as isize
    }

    fn from_tag(tag: isize) -> Option<(isize, QueueAction)> {
        let action = QueueAction::ALL.into_iter().find(|a| *a as isize == tag % 16)?;
        Some((tag / 16, action))
    }
}

/// The line above the rows while several transfers are listed.
struct Header {
    view: Retained<NSStackView>,
    summary: Retained<NSTextField>,
    pause_all: Retained<NSButton>,
}

pub struct ActivityIvars {
    stack: OnceCell<Retained<NSStackView>>,
    header: OnceCell<Header>,
    rows: RefCell<Vec<Row>>,
    next_id: std::cell::Cell<isize>,
    /// Transfers with a server end that may run at once.
    limit: std::cell::Cell<usize>,
    /// Ids of limited jobs that are running.
    running: RefCell<Vec<isize>>,
    /// Limited jobs waiting for a free slot, in order.
    waiting: RefCell<std::collections::VecDeque<(isize, Job, CancelToken)>>,
}

/// Uploads, downloads, and server-to-server copies count toward the limit
/// (each opens a connection); local work and deletions do not.
pub fn is_limited(job: &Job) -> bool {
    matches!(job.kind, JobKind::Copy | JobKind::Move) && (!job.source_fs.is_local() || !job.dest_fs.is_local())
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzActivity"]
    #[ivars = ActivityIvars]
    pub struct Activity;

    impl Activity {
        #[unsafe(method(stopJob:))]
        fn stop_job(&self, sender: &NSButton) {
            self.stop(sender.tag());
        }

        #[unsafe(method(pauseJob:))]
        fn pause_job(&self, sender: &NSButton) {
            self.toggle_pause(sender.tag());
        }

        #[unsafe(method(startNext:))]
        fn start_next(&self, sender: &NSButton) {
            self.move_to_front(sender.tag());
        }

        #[unsafe(method(pauseAll:))]
        fn pause_all_action(&self, _sender: &NSButton) {
            let all_paused = self.all_paused();
            self.pause_all(!all_paused);
        }

        #[unsafe(method(stopAll:))]
        fn stop_all_action(&self, _sender: &NSButton) {
            self.stop_all();
        }

        #[unsafe(method(queueAction:))]
        fn queue_action(&self, sender: &NSMenuItem) {
            if let Some((id, action)) = QueueAction::from_tag(sender.tag()) {
                self.perform(id, action);
            }
        }
    }

    unsafe impl NSObjectProtocol for Activity {}

    unsafe impl NSMenuDelegate for Activity {
        /// A transfer row's menu, built when it opens.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.fill_row_menu(menu);
        }
    }
);

impl Activity {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ActivityIvars {
            stack: OnceCell::new(),
            header: OnceCell::new(),
            rows: RefCell::new(Vec::new()),
            next_id: std::cell::Cell::new(1),
            limit: std::cell::Cell::new(2),
            running: RefCell::new(Vec::new()),
            waiting: RefCell::new(std::collections::VecDeque::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let stack = NSStackView::new(mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        stack.setSpacing(6.0);
        stack.setEdgeInsets(NSEdgeInsets { top: 6.0, left: 10.0, bottom: 8.0, right: 10.0 });
        stack.setHidden(true);
        let summary = small_label("", mtm);
        summary.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        let pause_all = NSButton::new(mtm);
        pause_all.setTitle(&ns("Pause All"));
        pause_all.setBezelStyle(NSBezelStyle::Push);
        pause_all.setControlSize(objc2_app_kit::NSControlSize::Small);
        let stop_all = NSButton::new(mtm);
        stop_all.setTitle(&ns("Stop All"));
        stop_all.setBezelStyle(NSBezelStyle::Push);
        stop_all.setControlSize(objc2_app_kit::NSControlSize::Small);
        unsafe {
            pause_all.setTarget(Some(&*this));
            pause_all.setAction(Some(sel!(pauseAll:)));
            stop_all.setTarget(Some(&*this));
            stop_all.setAction(Some(sel!(stopAll:)));
        }
        summary.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
        let header = NSStackView::stackViewWithViews(
            &objc2_foundation::NSArray::from_slice(&[&*summary as &NSView, &pause_all, &stop_all]),
            mtm,
        );
        header.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        header.setSpacing(8.0);
        header.setHidden(true);
        stack.addArrangedSubview(&header);
        header.leadingAnchor().constraintEqualToAnchor_constant(&stack.leadingAnchor(), 10.0).setActive(true);
        header.trailingAnchor().constraintEqualToAnchor_constant(&stack.trailingAnchor(), -10.0).setActive(true);
        let _ = this.ivars().header.set(Header { view: header, summary, pause_all });
        let _ = this.ivars().stack.set(stack);
        this
    }

    pub fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.ivars().stack.get().expect("built").clone())
    }

    /// Starts a job on a worker thread, with a row for it.
    pub fn start(&self, mut job: Job) {
        // Copies leave out what the filename filters name.
        if job.kind == JobKind::Copy && job.exclude.is_none() {
            job.exclude = super::transfer_exclusions();
        }
        if job.sources.is_empty() {
            return;
        }
        let mtm = self.mtm();
        let id = self.ivars().next_id.get();
        self.ivars().next_id.set(id + 1);
        let cancel = CancelToken::default();

        let title = label(&job.title(), mtm);
        let detail = small_label("Preparing\u{2026}", mtm);
        detail.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        let bar = NSProgressIndicator::new(mtm);
        bar.setStyle(NSProgressIndicatorStyle::Bar);
        bar.setIndeterminate(false);
        bar.setMinValue(0.0);
        bar.setMaxValue(1.0);
        let stop = NSButton::new(mtm);
        stop.setBezelStyle(NSBezelStyle::Circular);
        stop.setBordered(false);
        if let Some(image) = symbol("xmark.circle.fill", "Stop") {
            stop.setImage(Some(&image));
        } else {
            stop.setTitle(&ns("Stop"));
        }
        stop.setToolTip(Some(&ns("Stop")));
        stop.setTag(id);
        unsafe {
            stop.setTarget(Some(self));
            stop.setAction(Some(sel!(stopJob:)));
        }

        let texts = NSStackView::stackViewWithViews(
            &objc2_foundation::NSArray::from_slice(&[&*title as &NSView, &bar, &detail]),
            mtm,
        );
        texts.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        texts.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
        texts.setSpacing(2.0);
        bar.leadingAnchor().constraintEqualToAnchor(&texts.leadingAnchor()).setActive(true);
        bar.trailingAnchor().constraintEqualToAnchor(&texts.trailingAnchor()).setActive(true);
        let pause = icon_button("pause.circle.fill", "Pause", id, self, sel!(pauseJob:), mtm);
        let front = icon_button("arrow.up.circle.fill", "Start next", id, self, sel!(startNext:), mtm);
        front.setHidden(true);
        let row = NSStackView::stackViewWithViews(
            &objc2_foundation::NSArray::from_slice(&[&*texts as &NSView, &front, &pause, &stop]),
            mtm,
        );
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(8.0);
        row.setDetachesHiddenViews(true);
        // The bar takes the width; Stop stays at the right.
        row.setDistribution(objc2_app_kit::NSStackViewDistribution::Fill);
        texts.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
        // Right-click: reorder, start now, pause, or stop.
        let menu = NSMenu::new(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        unsafe { row.setMenu(Some(&menu)) };

        let stack = self.ivars().stack.get().expect("built");
        stack.addArrangedSubview(&row);
        row.leadingAnchor().constraintEqualToAnchor_constant(&stack.leadingAnchor(), 10.0).setActive(true);
        row.trailingAnchor().constraintEqualToAnchor_constant(&stack.trailingAnchor(), -10.0).setActive(true);
        stack.setHidden(false);
        self.ivars().rows.borrow_mut().push(Row {
            id,
            view: row,
            detail,
            bar,
            cancel: cancel.clone(),
            pause: Some(pause),
            front: Some(front.clone()),
        });
        if is_limited(&job) && self.ivars().running.borrow().len() >= self.ivars().limit.get() {
            front.setHidden(false);
            self.ivars().waiting.borrow_mut().push_back((id, job, cancel));
            self.arrange();
            return;
        }
        self.launch(id, job, cancel);
    }

    fn launch(&self, id: isize, job: Job, cancel: CancelToken) {
        if is_limited(&job) {
            self.ivars().running.borrow_mut().push(id);
            self.set_detail(id, "Preparing\u{2026}");
        }
        if let Some(front) = self.ivars().rows.borrow().iter().find(|r| r.id == id).and_then(|r| r.front.clone()) {
            front.setHidden(true);
        }
        self.arrange();
        let on_error = super::current_settings().map(|s| s.transfers.on_error).unwrap_or_default();
        std::thread::Builder::new()
            .name(format!("forkz job {id}"))
            .spawn(move || {
                let mut observer = UiObserver { id, last_report: None, on_error, tries: Default::default() };
                let started = Instant::now();
                let outcome = ops::run(&job, &mut observer, cancel);
                let took = started.elapsed();
                DispatchQueue::main().exec_async(move || super::job_finished(id, job, outcome, took));
            })
            .expect("starting a job thread");
    }

    /// A row for work that is not a job (searching, compressing): a title,
    /// a spinning bar, a line for `set_detail`, and Stop, which cancels the
    /// token returned. Remove it with `remove`.
    pub fn begin_task(&self, text: &str) -> (isize, CancelToken) {
        let mtm = self.mtm();
        let id = self.ivars().next_id.get();
        self.ivars().next_id.set(id + 1);
        let cancel = CancelToken::default();
        let title = label(text, mtm);
        let bar = NSProgressIndicator::new(mtm);
        bar.setStyle(NSProgressIndicatorStyle::Bar);
        bar.setIndeterminate(true);
        unsafe { bar.startAnimation(None) };
        let detail = small_label("", mtm);
        detail.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        let stop = NSButton::new(mtm);
        stop.setBezelStyle(NSBezelStyle::Circular);
        stop.setBordered(false);
        if let Some(image) = symbol("xmark.circle.fill", "Stop") {
            stop.setImage(Some(&image));
        } else {
            stop.setTitle(&ns("Stop"));
        }
        stop.setToolTip(Some(&ns("Stop")));
        stop.setTag(id);
        unsafe {
            stop.setTarget(Some(self));
            stop.setAction(Some(sel!(stopJob:)));
        }
        let texts = NSStackView::stackViewWithViews(
            &objc2_foundation::NSArray::from_slice(&[&*title as &NSView, &bar, &detail]),
            mtm,
        );
        texts.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        texts.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
        texts.setSpacing(2.0);
        bar.leadingAnchor().constraintEqualToAnchor(&texts.leadingAnchor()).setActive(true);
        bar.trailingAnchor().constraintEqualToAnchor(&texts.trailingAnchor()).setActive(true);
        let row =
            NSStackView::stackViewWithViews(&objc2_foundation::NSArray::from_slice(&[&*texts as &NSView, &stop]), mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row.setSpacing(8.0);
        // The bar takes the width; Stop stays at the right.
        row.setDistribution(objc2_app_kit::NSStackViewDistribution::Fill);
        texts.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
        let stack = self.ivars().stack.get().expect("built");
        stack.addArrangedSubview(&row);
        row.leadingAnchor().constraintEqualToAnchor_constant(&stack.leadingAnchor(), 10.0).setActive(true);
        row.trailingAnchor().constraintEqualToAnchor_constant(&stack.trailingAnchor(), -10.0).setActive(true);
        stack.setHidden(false);
        self.ivars().rows.borrow_mut().push(Row {
            id,
            view: row,
            detail,
            bar,
            cancel: cancel.clone(),
            pause: None,
            front: None,
        });
        (id, cancel)
    }

    /// The line under a row's bar (ignored once Stop was clicked).
    pub fn set_detail(&self, id: isize, text: &str) {
        if self.ivars().rows.borrow().iter().any(|r| r.id == id && r.cancel.is_cancelled()) {
            return;
        }
        self.set_detail_always(id, text);
    }

    fn set_detail_always(&self, id: isize, text: &str) {
        if let Some(row) = self.ivars().rows.borrow().iter().find(|r| r.id == id) {
            row.detail.setStringValue(&ns(text));
        }
    }

    /// Stop: a running job is cancelled (it ends on its own); a waiting
    /// one is dropped.
    fn stop(&self, id: isize) {
        let waiting = {
            let mut queue = self.ivars().waiting.borrow_mut();
            let position = queue.iter().position(|(queued, _, _)| *queued == id);
            position.and_then(|p| queue.remove(p))
        };
        if waiting.is_some() {
            self.remove(id);
            self.arrange();
            return;
        }
        if let Some(row) = self.ivars().rows.borrow().iter().find(|r| r.id == id) {
            row.cancel.cancel();
        }
        self.set_detail_always(id, "Stopping\u{2026}");
    }

    /// Pause and Resume: the job holds at its next chunk or item.
    fn toggle_pause(&self, id: isize) {
        let paused = self.ivars().rows.borrow().iter().find(|r| r.id == id).map(|r| !r.cancel.is_paused());
        if let Some(paused) = paused {
            self.set_paused(id, paused);
        }
        self.update_header();
    }

    fn set_paused(&self, id: isize, paused: bool) {
        let rows = self.ivars().rows.borrow();
        let Some(row) = rows.iter().find(|r| r.id == id) else { return };
        row.cancel.set_paused(paused);
        if let Some(button) = &row.pause {
            let (name, tip) = if paused { ("play.circle.fill", "Resume") } else { ("pause.circle.fill", "Pause") };
            if let Some(image) = symbol(name, tip) {
                button.setImage(Some(&image));
            }
            button.setToolTip(Some(&ns(tip)));
        }
        if paused {
            row.detail.setStringValue(&ns("Paused"));
        } else if self.is_waiting(id) {
            drop(rows);
            self.arrange();
        }
    }

    /// Start Next: a waiting job goes to the front of the queue.
    fn move_to_front(&self, id: isize) {
        self.move_in_queue(id, |_, _| 0);
    }

    /// Moves a waiting job to the place `to` picks (from its place and the
    /// queue's length).
    fn move_in_queue(&self, id: isize, to: impl Fn(usize, usize) -> usize) {
        let mut queue = self.ivars().waiting.borrow_mut();
        if let Some(position) = queue.iter().position(|(queued, _, _)| *queued == id)
            && let Some(entry) = queue.remove(position)
        {
            let place = to(position, queue.len() + 1).min(queue.len());
            queue.insert(place, entry);
        }
        drop(queue);
        self.arrange();
    }

    fn is_waiting(&self, id: isize) -> bool {
        self.ivars().waiting.borrow().iter().any(|(queued, _, _)| *queued == id)
    }

    /// Start Now: a waiting job runs right away, beyond the limit.
    fn start_now(&self, id: isize) {
        let entry = {
            let mut queue = self.ivars().waiting.borrow_mut();
            let position = queue.iter().position(|(queued, _, _)| *queued == id);
            position.and_then(|p| queue.remove(p))
        };
        if let Some((id, job, cancel)) = entry {
            self.launch(id, job, cancel);
        }
    }

    /// Ids of the rows with a job (transfers and other file work).
    fn job_ids(&self) -> Vec<isize> {
        self.ivars().rows.borrow().iter().filter(|r| r.pause.is_some()).map(|r| r.id).collect()
    }

    fn all_paused(&self) -> bool {
        let rows = self.ivars().rows.borrow();
        let mut jobs = rows.iter().filter(|r| r.pause.is_some()).peekable();
        jobs.peek().is_some() && jobs.all(|r| r.cancel.is_paused())
    }

    fn pause_all(&self, paused: bool) {
        for id in self.job_ids() {
            self.set_paused(id, paused);
        }
        self.update_header();
    }

    fn stop_all(&self) {
        for id in self.job_ids() {
            self.stop(id);
        }
    }

    fn perform(&self, id: isize, action: QueueAction) {
        match action {
            QueueAction::StartNow => self.start_now(id),
            QueueAction::ToTop => self.move_in_queue(id, |_, _| 0),
            QueueAction::Up => self.move_in_queue(id, |at, _| at.saturating_sub(1)),
            QueueAction::Down => self.move_in_queue(id, |at, _| at + 1),
            QueueAction::ToBottom => self.move_in_queue(id, |_, len| len),
            QueueAction::Pause => self.toggle_pause(id),
            QueueAction::Stop => self.stop(id),
            QueueAction::PauseAll => self.pause_all(true),
            QueueAction::ResumeAll => self.pause_all(false),
            QueueAction::StopAll => self.stop_all(),
        }
    }

    /// The menu of the row it belongs to.
    fn fill_row_menu(&self, menu: &NSMenu) {
        menu.removeAllItems();
        let id = {
            let rows = self.ivars().rows.borrow();
            rows.iter().find(|r| r.view.menu().is_some_and(|m| std::ptr::eq(&*m, menu))).map(|r| r.id)
        };
        let Some(id) = id else { return };
        let mtm = self.mtm();
        let add = |title: &str, action: QueueAction, enabled: bool| {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &ns(title),
                    Some(sel!(queueAction:)),
                    &ns(""),
                )
            };
            item.setTag(action.tag(id));
            unsafe { item.setTarget(Some(self)) };
            item.setEnabled(enabled);
            menu.addItem(&item);
        };
        menu.setAutoenablesItems(false);
        let place = self.ivars().waiting.borrow().iter().position(|(queued, _, _)| *queued == id);
        if let Some(place) = place {
            let last = self.ivars().waiting.borrow().len().saturating_sub(1);
            add("Start Now", QueueAction::StartNow, true);
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            add("Move to Top", QueueAction::ToTop, place > 0);
            add("Move Up", QueueAction::Up, place > 0);
            add("Move Down", QueueAction::Down, place < last);
            add("Move to Bottom", QueueAction::ToBottom, place < last);
            menu.addItem(&NSMenuItem::separatorItem(mtm));
        }
        let paused = self.ivars().rows.borrow().iter().any(|r| r.id == id && r.cancel.is_paused());
        add(if paused { "Resume" } else { "Pause" }, QueueAction::Pause, true);
        add("Stop", QueueAction::Stop, true);
        if self.job_ids().len() > 1 {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            add("Pause All", QueueAction::PauseAll, !self.all_paused());
            add("Resume All", QueueAction::ResumeAll, self.ivars().rows.borrow().iter().any(|r| r.cancel.is_paused()));
            add("Stop All", QueueAction::StopAll, true);
        }
    }

    /// Orders the rows: other work and running jobs first, then the waiting
    /// ones in queue order (each told its place); updates the header.
    fn arrange(&self) {
        let stack = self.ivars().stack.get().expect("built");
        let waiting: Vec<isize> = self.ivars().waiting.borrow().iter().map(|(id, _, _)| *id).collect();
        {
            let rows = self.ivars().rows.borrow();
            let mut order: Vec<&Row> = rows.iter().filter(|r| !waiting.contains(&r.id)).collect();
            order.extend(waiting.iter().filter_map(|id| rows.iter().find(|r| r.id == *id)));
            for row in order {
                stack.removeArrangedSubview(&row.view);
                stack.addArrangedSubview(&row.view);
            }
        }
        for (place, id) in waiting.iter().enumerate() {
            let paused = self.ivars().rows.borrow().iter().any(|r| r.id == *id && r.cancel.is_paused());
            if !paused {
                let text = match place {
                    0 => "Next in line, waiting for a transfer to finish\u{2026}".to_string(),
                    n => format!("Waiting, number {} in line\u{2026}", n + 1),
                };
                self.set_detail(*id, &text);
            }
        }
        self.update_header();
    }

    /// Shows "3 transfers, 1 waiting" with Pause All and Stop All while
    /// more than one job is listed.
    fn update_header(&self) {
        let Some(header) = self.ivars().header.get() else { return };
        let jobs = self.job_ids().len();
        header.view.setHidden(jobs < 2);
        if jobs < 2 {
            return;
        }
        let waiting = self.ivars().waiting.borrow().len();
        let mut text = format!("{jobs} jobs");
        if waiting > 0 {
            text.push_str(&format!(", {waiting} waiting"));
        }
        if self.all_paused() {
            text.push_str(", paused");
        }
        header.summary.setStringValue(&ns(&text));
        header.pause_all.setTitle(&ns(if self.all_paused() { "Resume All" } else { "Pause All" }));
    }

    /// A job ended: frees its slot and starts the next waiting one.
    pub fn job_done(&self, id: isize) {
        self.ivars().running.borrow_mut().retain(|running| *running != id);
        self.start_waiting();
    }

    pub fn limit(&self) -> usize {
        self.ivars().limit.get()
    }

    /// Changes how many transfers run at once (at least 1).
    pub fn set_limit(&self, limit: usize) {
        self.ivars().limit.set(limit.max(1));
        self.start_waiting();
    }

    fn start_waiting(&self) {
        while self.ivars().running.borrow().len() < self.ivars().limit.get() {
            let Some((id, job, cancel)) = self.ivars().waiting.borrow_mut().pop_front() else { break };
            self.launch(id, job, cancel);
        }
    }

    /// A row with a spinning bar and no job (such as "Connecting..."); remove
    /// it with `remove`.
    pub fn begin_note(&self, text: &str) -> isize {
        let mtm = self.mtm();
        let id = self.ivars().next_id.get();
        self.ivars().next_id.set(id + 1);
        let title = label(text, mtm);
        let bar = NSProgressIndicator::new(mtm);
        bar.setStyle(NSProgressIndicatorStyle::Bar);
        bar.setIndeterminate(true);
        unsafe { bar.startAnimation(None) };
        let detail = small_label("", mtm);
        let row =
            NSStackView::stackViewWithViews(&objc2_foundation::NSArray::from_slice(&[&*title as &NSView, &bar]), mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        row.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
        row.setSpacing(2.0);
        let stack = self.ivars().stack.get().expect("built");
        stack.addArrangedSubview(&row);
        row.leadingAnchor().constraintEqualToAnchor_constant(&stack.leadingAnchor(), 10.0).setActive(true);
        row.trailingAnchor().constraintEqualToAnchor_constant(&stack.trailingAnchor(), -10.0).setActive(true);
        bar.trailingAnchor().constraintEqualToAnchor(&row.trailingAnchor()).setActive(true);
        stack.setHidden(false);
        self.ivars().rows.borrow_mut().push(Row {
            id,
            view: row,
            detail,
            bar,
            cancel: CancelToken::default(),
            pause: None,
            front: None,
        });
        id
    }

    fn update(&self, id: isize, progress: &Progress) {
        let rows = self.ivars().rows.borrow();
        let Some(row) = rows.iter().find(|r| r.id == id) else { return };
        if row.cancel.is_cancelled() {
            return;
        }
        row.bar.setDoubleValue(progress.fraction());
        if row.cancel.is_paused() {
            row.detail.setStringValue(&ns("Paused"));
            return;
        }
        row.detail.setStringValue(&ns(&describe(progress)));
    }

    /// Removes a finished job's row.
    pub fn remove(&self, id: isize) {
        let mut rows = self.ivars().rows.borrow_mut();
        if let Some(index) = rows.iter().position(|r| r.id == id) {
            let row = rows.remove(index);
            row.view.removeFromSuperview();
        }
        if rows.is_empty() {
            self.ivars().stack.get().expect("built").setHidden(true);
        }
        drop(rows);
        self.update_header();
    }
}

/// "12 MB of 340 MB, 3 of 18 items: photo.jpg"
fn describe(progress: &Progress) -> String {
    let mut text = if progress.total_bytes > 0 {
        format!("{} of {}", format_size(progress.done_bytes), format_size(progress.total_bytes))
    } else {
        format!("{} of {} items", progress.done_items.min(progress.total_items), progress.total_items)
    };
    if !progress.current.is_empty() {
        text.push_str(&format!(": {}", progress.current));
    }
    text
}

/// Reports to the main thread; blocks the worker while the user answers.
struct UiObserver {
    id: isize,
    last_report: Option<Instant>,
    /// The error rule from settings: "ask", "skip", or "retry".
    on_error: String,
    /// Tries so far per item (for "retry").
    tries: std::collections::HashMap<std::path::PathBuf, u32>,
}

impl Observer for UiObserver {
    fn progress(&mut self, progress: &Progress) {
        // At most 20 updates a second, plus the last one.
        let now = Instant::now();
        let done = progress.done_items >= progress.total_items && progress.done_bytes >= progress.total_bytes;
        if !done && self.last_report.is_some_and(|t| now - t < Duration::from_millis(50)) {
            return;
        }
        self.last_report = Some(now);
        let (id, progress) = (self.id, progress.clone());
        DispatchQueue::main().exec_async(move || {
            super::with_activity(|activity| activity.update(id, &progress));
        });
    }

    fn conflict(&mut self, conflict: &Conflict) -> Answer {
        let (reply, answer) = mpsc::channel();
        let conflict = conflict.clone();
        DispatchQueue::main().exec_async(move || dialogs::ask_conflict(conflict, reply));
        answer.recv().unwrap_or(Answer { resolution: ops::Resolution::Stop, apply_to_all: false })
    }

    fn error(&mut self, path: &Path, error: &std::io::Error) -> ErrorAnswer {
        match self.on_error.as_str() {
            "skip" => return ErrorAnswer::Skip,
            "retry" => {
                let tries = self.tries.entry(path.to_path_buf()).or_insert(0);
                *tries += 1;
                if *tries <= 3 {
                    // A moment for a dropped connection to come back.
                    std::thread::sleep(Duration::from_secs(u64::from(*tries)));
                    return ErrorAnswer::Retry;
                }
                return ErrorAnswer::Skip;
            }
            _ => {}
        }
        let (reply, answer) = mpsc::channel();
        let (path, error) = (path.to_path_buf(), error.to_string());
        DispatchQueue::main().exec_async(move || dialogs::ask_error(&path, error, reply));
        answer.recv().unwrap_or(ErrorAnswer::Stop)
    }
}

/// A summary of what did not work, if anything.
pub fn report_problems(outcome: &Outcome) {
    if outcome.failed.is_empty() {
        return;
    }
    let lines: Vec<String> = outcome
        .failed
        .iter()
        .take(8)
        .map(|(path, error)| format!("\u{201c}{}\u{201d}: {error}", crate::pane::title_of(path)))
        .collect();
    let mut detail = lines.join("\n");
    if outcome.failed.len() > lines.len() {
        detail.push_str(&format!("\n\u{2026}and {} more", outcome.failed.len() - lines.len()));
    }
    let count = outcome.failed.len();
    let message = if count == 1 {
        "1 item could not be processed.".to_string()
    } else {
        format!("{count} items could not be processed.")
    };
    dialogs::show_error(message, detail);
}
