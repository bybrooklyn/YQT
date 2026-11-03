// SPDX-License-Identifier: GPL-3.0-only
//! The `QueueModel` QML singleton: a `QAbstractListModel` (subclassed from Rust through cxx-qt) that mirrors the
//! play queue. The `Player` owns the [`QueueState`](crate::models::queue::QueueState) and calls [`publish`] with
//! each [`Change`]; QML's requests (`moveRow`, `removeRow`, …) travel back through [`install_editor`].
//! (This file lives in `src/` rather than `src/models/` because cxx-qt-build needs all bridge files of one QML
//! module in a single directory.)

use crate::models::queue::QueueRow;
use crate::models::Change;
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};
use serde_json::Value;
use std::sync::Mutex;

const ROLE_BASE: i32 = 0x100; // Qt::UserRole
const ROLE_NAMES: [&str; 8] = ["title", "artist", "thumbnail", "duration", "videoId", "artistId", "albumId", "kind"];

/// An edit requested from QML, run by whoever owns the queue.
pub type Edit = Box<dyn FnOnce(&mut dyn QueueEditor) + Send>;

/// The operations the model can ask the owner of the queue to perform.
pub trait QueueEditor {
    fn move_row(&mut self, from: usize, to: usize);
    fn remove_row(&mut self, at: usize);
    fn play_row(&mut self, at: usize);
    fn play_row_next(&mut self, at: usize);
}

/// Hand-off from the model to the queue's owner (installed by the `Player`; absent in harnesses).
type EditRunner = Box<dyn Fn(Edit) + Send>;
static EDITOR: Mutex<Option<EditRunner>> = Mutex::new(None);

pub fn install_editor(f: impl Fn(Edit) + Send + 'static) {
    *EDITOR.lock().unwrap() = Some(Box::new(f));
}

fn edit(f: impl FnOnce(&mut dyn QueueEditor) + Send + 'static) {
    if let Some(run) = EDITOR.lock().unwrap().as_ref() {
        run(Box::new(f));
    }
}

/// Rows published before the model exists (QML creates the singleton lazily); the model adopts them on creation.
static PENDING: Mutex<Option<Vec<QueueRow>>> = Mutex::new(None);
static LIVE: Mutex<Option<cxx_qt::CxxQtThread<qobject::QueueModel>>> = Mutex::new(None);

/// Called by the `Player` after every queue edit; `snapshot` is only used when the model is not alive yet.
pub fn publish(change: Change<Value>, snapshot: impl FnOnce() -> Vec<Value>) {
    let rows_change = match change {
        Change::Reset(rows) => Change::Reset(rows.iter().map(QueueRow::from_value).collect()),
        Change::Insert { at, rows } => Change::Insert { at, rows: rows.iter().map(QueueRow::from_value).collect() },
        Change::Remove { at, count } => Change::Remove { at, count },
        Change::Move { from, to } => Change::Move { from, to },
        Change::Update { at, row } => Change::Update { at, row: QueueRow::from_value(&row) },
    };
    let live = LIVE.lock().unwrap();
    match live.as_ref() {
        Some(th) => {
            let _ = th.queue(move |mut m| {
                crate::apply_to_model!(m, rows_change, &mut m.as_mut().rust_mut().get_mut().rows, [ROLE_BASE, ROLE_BASE + 1]);
                m.sync_count();
            });
        }
        None => *PENDING.lock().unwrap() = Some(snapshot().iter().map(QueueRow::from_value).collect()),
    }
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++Qt" {
        include!(<QtCore/QAbstractListModel>);
        #[qobject]
        type QAbstractListModel;
    }

    unsafe extern "C++" {
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvector.h");
        type QVector_i32 = cxx_qt_lib::QVector<i32>;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(i32, count)]
        type QueueModel = super::QueueModelRust;

        /// Drop row `from` onto position `to` (the drag-and-drop commit). The playing track keeps playing.
        #[qinvokable]
        fn move_row(self: Pin<&mut QueueModel>, from: i32, to: i32);
        #[qinvokable]
        fn remove_row(self: Pin<&mut QueueModel>, at: i32);
        /// Make this row the playing one.
        #[qinvokable]
        fn play_row(self: Pin<&mut QueueModel>, at: i32);
        /// Move this row to just after the playing one.
        #[qinvokable]
        fn play_row_next(self: Pin<&mut QueueModel>, at: i32);

        #[inherit]
        #[cxx_name = "beginInsertRows"]
        unsafe fn begin_insert_rows(self: Pin<&mut QueueModel>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        #[cxx_name = "endInsertRows"]
        unsafe fn end_insert_rows(self: Pin<&mut QueueModel>);
        #[inherit]
        #[cxx_name = "beginRemoveRows"]
        unsafe fn begin_remove_rows(self: Pin<&mut QueueModel>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        #[cxx_name = "endRemoveRows"]
        unsafe fn end_remove_rows(self: Pin<&mut QueueModel>);
        #[inherit]
        #[cxx_name = "beginMoveRows"]
        unsafe fn begin_move_rows(
            self: Pin<&mut QueueModel>,
            source_parent: &QModelIndex,
            source_first: i32,
            source_last: i32,
            dest_parent: &QModelIndex,
            dest_child: i32,
        ) -> bool;
        #[inherit]
        #[cxx_name = "endMoveRows"]
        unsafe fn end_move_rows(self: Pin<&mut QueueModel>);
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut QueueModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut QueueModel>);
        #[inherit]
        #[cxx_name = "dataChanged"]
        #[qsignal]
        fn data_changed(self: Pin<&mut QueueModel>, top_left: &QModelIndex, bottom_right: &QModelIndex, roles: &QVector_i32);
        #[inherit]
        fn index(self: &QueueModel, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;

        #[qinvokable]
        #[cxx_override]
        fn data(self: &QueueModel, index: &QModelIndex, role: i32) -> QVariant;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &QueueModel) -> QHash_i32_QByteArray;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &QueueModel, parent: &QModelIndex) -> i32;
    }

    impl cxx_qt::Threading for QueueModel {}
    impl cxx_qt::Initialize for QueueModel {}
}

#[derive(Default)]
pub struct QueueModelRust {
    count: i32,
    rows: Vec<QueueRow>,
}

impl cxx_qt::Initialize for qobject::QueueModel {
    fn initialize(mut self: Pin<&mut Self>) {
        let rows = PENDING.lock().unwrap().take().unwrap_or_default();
        self.as_mut().rust_mut().get_mut().rows = rows;
        let n = self.rows.len() as i32;
        self.as_mut().set_count(n);
        *LIVE.lock().unwrap() = Some(self.qt_thread());
    }
}

impl qobject::QueueModel {
    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(row) = usize::try_from(index.row()).ok().and_then(|i| self.rows.get(i)) else { return QVariant::default() };
        let text = |s: &str| QVariant::from(&QString::from(s));
        match role - ROLE_BASE {
            0 => text(&row.title),
            1 => text(&row.artist),
            2 => text(&row.thumbnail),
            3 => text(&row.duration),
            4 => text(&row.video_id),
            5 => text(&row.artist_id),
            6 => text(&row.album_id),
            7 => text("song"),
            _ => QVariant::default(),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        for (i, name) in ROLE_NAMES.iter().enumerate() {
            roles.insert(ROLE_BASE + i as i32, QByteArray::from(*name));
        }
        roles
    }

    pub fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.rows.len() as i32
    }

    pub fn move_row(self: Pin<&mut Self>, from: i32, to: i32) {
        if let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) {
            edit(move |q| q.move_row(from, to));
        }
    }
    pub fn remove_row(self: Pin<&mut Self>, at: i32) {
        if let Ok(at) = usize::try_from(at) {
            edit(move |q| q.remove_row(at));
        }
    }
    pub fn play_row(self: Pin<&mut Self>, at: i32) {
        if let Ok(at) = usize::try_from(at) {
            edit(move |q| q.play_row(at));
        }
    }
    pub fn play_row_next(self: Pin<&mut Self>, at: i32) {
        if let Ok(at) = usize::try_from(at) {
            edit(move |q| q.play_row_next(at));
        }
    }

    /// Keep the `count` property in step after a change was applied.
    pub fn sync_count(mut self: Pin<&mut Self>) {
        let n = self.rows.len() as i32;
        if *self.count() != n {
            self.as_mut().set_count(n);
        }
    }
}
