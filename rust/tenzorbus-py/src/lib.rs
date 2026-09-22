//! Python bindings for the TenzorBus production ring.
//!
//! The API mirrors the shape the handoff specified:
//!
//! ```python
//! import tenzorbus_rs as tenzorbus
//!
//! bus      = tenzorbus.create("frames", slots=16, slot_bytes=1 << 20)
//! producer = bus.producer()
//! consumer = tenzorbus.attach("frames").consumer()
//!
//! with consumer.next() as lease:
//!     t = lease.torch()      # direct view, no consumer copy
//! ```
//!
//! Lifetime is explicit and enforced at runtime. A lease exports the slot
//! through the buffer protocol; NumPy and PyTorch views hold that export. While
//! any export is outstanding, `release()` refuses, so the v0.1 lifetime bug —
//! a NumPy view outliving its lease and then aliasing a recycled slot — cannot
//! be reproduced through these bindings. `lease.copy()` is the way to keep data
//! past the lease.

use std::os::raw::c_int;
use std::time::Duration;

use pyo3::exceptions::{PyBufferError, PyRuntimeError, PyTimeoutError, PyValueError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use tenzorbus::{Backpressure, DType, Error, Ring, RingOptions, SlotWriter, TensorView};

fn to_py_err(e: Error) -> PyErr {
    match e {
        Error::Timeout => PyTimeoutError::new_err(e.to_string()),
        Error::RingFull => PyRuntimeError::new_err(e.to_string()),
        Error::Invalid(m) => PyValueError::new_err(m),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Ring
// ---------------------------------------------------------------------------

#[pyclass(name = "Ring", unsendable)]
pub struct PyRing {
    ring: Ring,
}

#[pymethods]
impl PyRing {
    #[getter]
    fn name(&self) -> &str {
        self.ring.name()
    }

    #[getter]
    fn slot_count(&self) -> usize {
        self.ring.slot_count()
    }

    #[getter]
    fn slot_capacity(&self) -> usize {
        self.ring.slot_capacity()
    }

    fn producer(&self) -> PyResult<PyProducer> {
        let p = self.ring.producer().map_err(to_py_err)?;
        Ok(PyProducer { producer: p })
    }

    fn consumer(&self) -> PyResult<PyConsumer> {
        let c = self.ring.consumer().map_err(to_py_err)?;
        Ok(PyConsumer { consumer: c })
    }

    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let s = self.ring.stats();
        let d = PyDict::new(py);
        d.set_item("name", self.ring.name())?;
        d.set_item("slot_count", s.slot_count)?;
        d.set_item("slot_capacity", s.slot_capacity)?;
        d.set_item("next_sequence", s.next_sequence)?;
        d.set_item("consumers", s.consumers)?;
        d.set_item("published", s.published)?;
        d.set_item("dropped", s.dropped)?;
        d.set_item("reaped", s.reaped)?;
        d.set_item("free_slots", s.free_slots)?;
        d.set_item("committed_slots", s.committed_slots)?;
        Ok(d)
    }

    /// Run a liveness sweep: reap consumers whose process is gone and reclaim
    /// slots abandoned by a dead producer.
    fn sweep(&self) {
        self.ring.sweep();
    }

    /// Remove the shared object when this handle is dropped (the default for a
    /// ring this process created).
    fn unlink_on_close(&self) {
        self.ring.unlink_on_drop();
    }

    /// Leave the shared object in place when this handle is dropped.
    fn keep_on_close(&self) {
        self.ring.keep_on_drop();
    }

    fn __repr__(&self) -> String {
        format!(
            "<tenzorbus.Ring name={} slots={} slot_bytes={}>",
            self.ring.name(),
            self.ring.slot_count(),
            self.ring.slot_capacity()
        )
    }
}

// ---------------------------------------------------------------------------
// Producer
// ---------------------------------------------------------------------------

#[pyclass(name = "Producer", unsendable)]
pub struct PyProducer {
    producer: tenzorbus::Producer,
}

/// Borrow an arbitrary buffer-protocol object as flat bytes for the duration
/// of the call. Returns the raw view so the caller can release it.
struct FlatBuffer {
    view: ffi::Py_buffer,
}

impl FlatBuffer {
    fn acquire(obj: &Bound<'_, PyAny>) -> PyResult<Self> {
        let mut view: ffi::Py_buffer = unsafe { std::mem::zeroed() };
        let rc =
            unsafe { ffi::PyObject_GetBuffer(obj.as_ptr(), &mut view, ffi::PyBUF_SIMPLE as c_int) };
        if rc != 0 {
            return Err(PyErr::fetch(obj.py()));
        }
        Ok(FlatBuffer { view })
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.view.buf as *const u8, self.view.len as usize) }
    }
}

impl Drop for FlatBuffer {
    fn drop(&mut self) {
        unsafe { ffi::PyBuffer_Release(&mut self.view) };
    }
}

#[pymethods]
impl PyProducer {
    /// Copy `array` into a slot and commit it.
    ///
    /// `timestamp_ns` carries the source's own capture time into the slot header.
    /// Leave it unset only for synthetic data; a media pipeline must pass it, or
    /// the slot records wall-clock time at publish instead of media time.
    ///
    /// Returns a dict describing the publication, or `None` when the policy is
    /// `drop_newest` and no slot was reclaimable.
    #[pyo3(signature = (array, *, policy = "block", timeout = 1.0, timestamp_ns = None))]
    fn publish<'py>(
        &self,
        py: Python<'py>,
        array: &Bound<'py, PyAny>,
        policy: &str,
        timeout: f64,
        timestamp_ns: Option<u64>,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let policy = match policy {
            "block" => Backpressure::Block,
            "drop_newest" => Backpressure::DropNewest,
            other => {
                return Err(PyValueError::new_err(format!(
                    "policy must be 'block' or 'drop_newest', got '{other}'"
                )));
            }
        };

        // Normalise through NumPy so shape/dtype/contiguity match the reference
        // implementation's contract exactly.
        let np = py.import("numpy")?;
        let arr = np.call_method1("ascontiguousarray", (array,))?;
        let dtype_name: String = arr.getattr("dtype")?.str()?.extract()?;
        let shape: Vec<u32> = arr.getattr("shape")?.extract()?;
        let dtype = DType::from_numpy_name(&dtype_name)
            .ok_or_else(|| PyValueError::new_err(format!("unsupported dtype {dtype_name}")))?;

        let buffer = FlatBuffer::acquire(&arr)?;
        let mut view =
            TensorView::contiguous(dtype, &shape, buffer.as_slice()).map_err(to_py_err)?;
        // A media producer passes the source's own capture time; without it the
        // slot carries wall-clock time at publish, which a consumer cannot use
        // to align a frame with the rest of the clip.
        if let Some(ns) = timestamp_ns {
            view = view.with_timestamp_ns(ns);
        }
        let result = self
            .producer
            .publish(&view, policy, Duration::from_secs_f64(timeout))
            .map_err(to_py_err)?;

        Ok(match result {
            None => None,
            Some(r) => {
                let d = PyDict::new(py);
                d.set_item("sequence", r.sequence)?;
                d.set_item("slot_index", r.slot_index)?;
                d.set_item("nbytes", r.nbytes)?;
                d.set_item("readers", r.readers)?;
                d.set_item("timestamp_ns", view.timestamp_ns())?;
                Some(d)
            }
        })
    }

    /// Phase 4B direct write: reserve a slot and fill it in place, with no
    /// producer copy.
    ///
    /// ```text
    /// with producer.reserve("float32", (3, 224, 224)) as w:
    ///     frame = w.numpy()          # writable view of the slot itself
    ///     decode_into(frame)         # no copy anywhere
    ///     w.set_timestamp_ns(ts)
    /// # committed on a clean exit, aborted if the block raised
    /// ```
    ///
    /// Returns None under the `drop_newest` policy when no slot was reclaimable.
    #[pyo3(signature = (dtype, shape, *, policy = "block", timeout = 1.0))]
    fn reserve(
        slf: PyRef<'_, Self>,
        dtype: &str,
        shape: Vec<u32>,
        policy: &str,
        timeout: f64,
    ) -> PyResult<Option<PySlotWriter>> {
        let policy = match policy {
            "block" => Backpressure::Block,
            "drop_newest" => Backpressure::DropNewest,
            other => {
                return Err(PyValueError::new_err(format!(
                    "policy must be 'block' or 'drop_newest', got '{other}'"
                )));
            }
        };
        let dt = DType::from_numpy_name(dtype)
            .ok_or_else(|| PyValueError::new_err(format!("unsupported dtype {dtype}")))?;
        // The returned writer borrows the Producer, which lives inside a Python
        // object. Python objects never move, so taking the address first and
        // holding a `Py<PyProducer>` afterwards keeps that borrow valid for as
        // long as the writer exists -- which is what lets it be widened to
        // 'static below. `PySlotWriter::drop` explicitly destroys the borrowed
        // writer before this owner; ordinary field destruction order is not
        // sufficient for this self-referential lifetime invariant.
        let address = (&slf.producer) as *const tenzorbus::Producer as usize;
        let owner: Py<PyProducer> = slf.into();
        let producer = unsafe { &*(address as *const tenzorbus::Producer) };
        let reserved = producer
            .reserve(dt, &shape, policy, Duration::from_secs_f64(timeout))
            .map_err(to_py_err)?;
        Ok(reserved.map(move |writer| {
            let leaked: SlotWriter<'static> = unsafe { std::mem::transmute(writer) };
            PySlotWriter {
                _producer: owner,
                writer: Some(leaked),
                dtype: dt,
                shape,
                exports: 0,
            }
        }))
    }
}

#[pyclass(name = "SlotWriter", unsendable)]
pub struct PySlotWriter {
    /// Keeps the Producer (and therefore the mapping) alive while this exists.
    _producer: Py<PyProducer>,
    writer: Option<SlotWriter<'static>>,
    dtype: DType,
    shape: Vec<u32>,
    exports: usize,
}

impl Drop for PySlotWriter {
    fn drop(&mut self) {
        // `writer` contains a lifetime-widened borrow into `_producer`. Rust
        // normally drops fields in declaration order, which would release the
        // owner (and potentially unmap the ring) before `SlotWriter::drop`
        // returns its reserved slot. Finish the borrow while its owner is
        // unquestionably alive. The remaining automatic drop sees `None`.
        drop(self.writer.take());
    }
}

impl PySlotWriter {
    fn writer(&mut self) -> PyResult<&mut SlotWriter<'static>> {
        self.writer
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("slot writer has already been finished"))
    }
}

#[pymethods]
impl PySlotWriter {
    #[getter]
    fn sequence(&self) -> PyResult<u64> {
        Ok(self
            .writer
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("slot writer has already been finished"))?
            .sequence())
    }

    #[getter]
    fn slot_index(&self) -> PyResult<usize> {
        Ok(self
            .writer
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("slot writer has already been finished"))?
            .slot_index())
    }

    #[getter]
    fn nbytes(&self) -> PyResult<usize> {
        Ok(self
            .writer
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("slot writer has already been finished"))?
            .nbytes())
    }

    /// Address of the slot payload, so a caller can prove no copy happened.
    #[getter]
    fn data_address(&mut self) -> PyResult<usize> {
        Ok(self.writer()?.payload().as_ptr() as usize)
    }

    fn set_timestamp_ns(&mut self, timestamp_ns: u64) -> PyResult<()> {
        self.writer()?.set_timestamp_ns(timestamp_ns);
        Ok(())
    }

    /// Writable NumPy array over the slot payload. Fill this instead of building
    /// a tensor elsewhere and handing it to `publish`.
    fn numpy<'py>(slf: Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let (dtype, shape) = {
            let borrowed = slf.borrow();
            (borrowed.dtype.numpy_name(), borrowed.shape.clone())
        };
        let np = py.import("numpy")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("dtype", dtype)?;
        let flat = np.call_method("frombuffer", (slf,), Some(&kwargs))?;
        flat.call_method1("reshape", (shape,))
    }

    /// Publish the slot.
    fn commit<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        if self.exports > 0 {
            return Err(PyBufferError::new_err(
                "cannot commit while a view of the slot is still alive; \
                 drop the array first, or the producer could mutate a published slot",
            ));
        }
        let writer = self
            .writer
            .take()
            .ok_or_else(|| PyRuntimeError::new_err("slot writer has already been finished"))?;
        let result = writer.commit();
        let d = PyDict::new(py);
        d.set_item("sequence", result.sequence)?;
        d.set_item("slot_index", result.slot_index)?;
        d.set_item("nbytes", result.nbytes)?;
        d.set_item("readers", result.readers)?;
        Ok(d)
    }

    /// Give the slot back without publishing.
    fn abort(&mut self) -> PyResult<()> {
        if self.exports > 0 {
            return Err(PyBufferError::new_err(
                "cannot abort while a view of the slot is still alive",
            ));
        }
        if let Some(writer) = self.writer.take() {
            writer.abort();
        }
        Ok(())
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (*args))]
    fn __exit__(&mut self, py: Python<'_>, args: &Bound<'_, PyTuple>) -> PyResult<bool> {
        let raised = args.get_item(0).map(|v| !v.is_none()).unwrap_or(false);
        if raised {
            self.abort()?;
        } else {
            self.commit(py)?;
        }
        Ok(false)
    }

    fn __len__(&self) -> PyResult<usize> {
        self.nbytes()
    }

    unsafe fn __getbuffer__(
        mut slf: PyRefMut<'_, Self>,
        view: *mut ffi::Py_buffer,
        flags: c_int,
    ) -> PyResult<()> {
        let (ptr, len) = {
            let w = slf.writer()?;
            (
                w.payload().as_mut_ptr() as *mut std::ffi::c_void,
                w.nbytes() as ffi::Py_ssize_t,
            )
        };
        let obj = slf.as_ptr();
        // Writable: filling the slot in place is the entire point of this path.
        let rc = unsafe { ffi::PyBuffer_FillInfo(view, obj, ptr, len, 0, flags) };
        if rc != 0 {
            return Err(PyErr::fetch(slf.py()));
        }
        slf.exports += 1;
        Ok(())
    }

    unsafe fn __releasebuffer__(mut slf: PyRefMut<'_, Self>, _view: *mut ffi::Py_buffer) {
        slf.exports = slf.exports.saturating_sub(1);
    }
}

// ---------------------------------------------------------------------------
// Consumer + Lease
// ---------------------------------------------------------------------------

#[pyclass(name = "Consumer", unsendable)]
pub struct PyConsumer {
    consumer: tenzorbus::Consumer,
}

#[pymethods]
impl PyConsumer {
    #[getter]
    fn index(&self) -> usize {
        self.consumer.slot_index()
    }

    #[getter]
    fn generation(&self) -> u32 {
        self.consumer.generation()
    }

    #[getter]
    fn last_sequence(&self) -> u64 {
        self.consumer.last_sequence()
    }

    /// Block until the next publication addressed to this consumer arrives.
    /// The GIL is released while waiting, so other Python threads run.
    #[pyo3(signature = (timeout = 1.0))]
    fn next(slf: PyRef<'_, Self>, timeout: f64) -> PyResult<PyLease> {
        let py = slf.py();
        // `Consumer` is not `Sync` (it keeps GIL-bound interior state), so the
        // handle and the payload pointer both cross the GIL release as plain
        // addresses. This is sound because `detach` runs the closure on this
        // same OS thread and the `PyRef` keeps the consumer alive throughout.
        let addr = (&slf.consumer) as *const tenzorbus::Consumer as usize;
        let taken = py.detach(move || unsafe {
            (*(addr as *const tenzorbus::Consumer))
                .next_raw(Duration::from_secs_f64(timeout))
                .map(|(meta, ptr, len)| (meta, ptr as usize, len))
        });
        let (meta, ptr, len) = taken.map_err(to_py_err)?;
        let ptr = ptr as *const u8;
        Ok(PyLease {
            consumer: slf.into(),
            meta: Some(meta),
            ptr,
            len,
            exports: 0,
        })
    }

    fn close(&self) {
        self.consumer.close();
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&self, _args: &Bound<'_, PyTuple>) -> bool {
        self.consumer.close();
        false
    }
}

#[pyclass(name = "Lease", unsendable)]
pub struct PyLease {
    consumer: Py<PyConsumer>,
    meta: Option<tenzorbus::TensorMeta>,
    ptr: *const u8,
    len: usize,
    exports: usize,
}

impl PyLease {
    fn meta(&self) -> PyResult<&tenzorbus::TensorMeta> {
        self.meta
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("lease has already been released"))
    }
}

#[pymethods]
impl PyLease {
    #[getter]
    fn sequence(&self) -> PyResult<u64> {
        Ok(self.meta()?.sequence)
    }

    #[getter]
    fn timestamp_ns(&self) -> PyResult<u64> {
        Ok(self.meta()?.timestamp_ns)
    }

    #[getter]
    fn nbytes(&self) -> PyResult<usize> {
        Ok(self.meta()?.nbytes)
    }

    #[getter]
    fn slot_index(&self) -> PyResult<usize> {
        Ok(self.meta()?.slot_index)
    }

    #[getter]
    fn readers_remaining(&self) -> PyResult<u32> {
        Ok(self.meta()?.readers_remaining)
    }

    #[getter]
    fn dtype(&self) -> PyResult<&'static str> {
        Ok(self.meta()?.dtype.numpy_name())
    }

    #[getter]
    fn shape(&self) -> PyResult<Vec<u32>> {
        Ok(self.meta()?.shape().to_vec())
    }

    #[getter]
    fn strides(&self) -> PyResult<Vec<u32>> {
        Ok(self.meta()?.strides().to_vec())
    }

    /// Address of the payload inside the shared mapping. Two consumers in the
    /// same process reading the same slot see the same address; a copy would
    /// not.
    #[getter]
    fn data_address(&self) -> PyResult<usize> {
        self.meta()?;
        Ok(self.ptr as usize)
    }

    /// Zero-copy NumPy view of the slot. Valid only while this lease is held.
    fn numpy<'py>(slf: Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let (dtype, shape) = {
            let borrowed = slf.borrow();
            let meta = borrowed.meta()?;
            (meta.dtype.numpy_name(), meta.shape().to_vec())
        };
        let np = py.import("numpy")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("dtype", dtype)?;
        let flat = np.call_method("frombuffer", (slf,), Some(&kwargs))?;
        flat.call_method1("reshape", (shape,))
    }

    /// Zero-copy PyTorch view of the slot. Valid only while this lease is held.
    ///
    /// Built from the NumPy view rather than from the lease directly. `torch.frombuffer`
    /// releases the `Py_buffer` as soon as it returns, so the lease's export count drops
    /// back to zero and `release()` would accept the lease while the tensor still aliased
    /// the slot -- the ring could then recycle memory the caller was reading.
    /// `torch.from_numpy` instead keeps the NumPy array alive, and that array holds the
    /// export, so the refusal in `release()` covers Torch views too.
    fn torch<'py>(slf: Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let view = PyLease::numpy(slf, py)?;
        let torch = py.import("torch")?;
        torch.call_method1("from_numpy", (view,))
    }

    /// Owned NumPy copy that outlives the lease.
    fn copy<'py>(slf: Bound<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let view = PyLease::numpy(slf, py)?;
        view.call_method0("copy")
    }

    /// Hand the slot back. Refuses while a NumPy/PyTorch view still exports it.
    fn release(&mut self, py: Python<'_>) -> PyResult<()> {
        if self.meta.is_none() {
            return Ok(());
        }
        if self.exports > 0 {
            return Err(PyBufferError::new_err(
                "cannot release a lease while a view still references the slot; \
                 drop the view or use lease.copy()",
            ));
        }
        let meta = self.meta.take().expect("checked above");
        let consumer = self.consumer.bind(py).borrow();
        consumer.consumer.release_raw(&meta).map_err(to_py_err)
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&mut self, py: Python<'_>, _args: &Bound<'_, PyTuple>) -> PyResult<bool> {
        self.release(py)?;
        Ok(false)
    }

    fn __len__(&self) -> PyResult<usize> {
        Ok(self.meta()?.nbytes)
    }

    fn __repr__(&self) -> String {
        match &self.meta {
            None => "<tenzorbus.Lease released>".to_string(),
            Some(m) => format!(
                "<tenzorbus.Lease seq={} slot={} dtype={} shape={:?}>",
                m.sequence,
                m.slot_index,
                m.dtype.numpy_name(),
                m.shape()
            ),
        }
    }

    unsafe fn __getbuffer__(
        mut slf: PyRefMut<'_, Self>,
        view: *mut ffi::Py_buffer,
        flags: c_int,
    ) -> PyResult<()> {
        if slf.meta.is_none() {
            return Err(PyBufferError::new_err("lease has already been released"));
        }
        let ptr = slf.ptr as *mut std::ffi::c_void;
        let len = slf.len as ffi::Py_ssize_t;
        let obj = slf.as_ptr();
        // Writable, matching the reference implementation's memoryview. The
        // single-producer contract still says consumers must not write.
        let rc = unsafe { ffi::PyBuffer_FillInfo(view, obj, ptr, len, 0, flags) };
        if rc != 0 {
            return Err(PyErr::fetch(slf.py()));
        }
        slf.exports += 1;
        Ok(())
    }

    unsafe fn __releasebuffer__(mut slf: PyRefMut<'_, Self>, _view: *mut ffi::Py_buffer) {
        slf.exports = slf.exports.saturating_sub(1);
    }
}

impl Drop for PyLease {
    fn drop(&mut self) {
        if let Some(meta) = self.meta.take() {
            Python::attach(|py| {
                let consumer = self.consumer.bind(py).borrow();
                let _ = consumer.consumer.release_raw(&meta);
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// Create a new ring.
///
/// By default, creation fails if `name` already exists. Pass `force=True` to
/// unlink that name and create a replacement. Existing handles keep using the
/// old mapping; subsequent `attach(name)` calls open the replacement.
#[pyfunction]
#[pyo3(signature = (name, *, slots = 8, slot_bytes = 1 << 20, force = false))]
fn create(name: &str, slots: usize, slot_bytes: usize, force: bool) -> PyResult<PyRing> {
    let ring = Ring::create(
        name,
        RingOptions {
            slot_count: slots,
            slot_capacity: slot_bytes,
            force,
        },
    )
    .map_err(to_py_err)?;
    Ok(PyRing { ring })
}

/// Attach to an existing ring created by another process.
#[pyfunction]
fn attach(name: &str) -> PyResult<PyRing> {
    let ring = Ring::attach(name).map_err(to_py_err)?;
    Ok(PyRing { ring })
}

/// Remove a ring's shared object by name.
#[pyfunction]
fn unlink(name: &str) -> PyResult<()> {
    tenzorbus::shm::unlink(name).map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

#[pymodule]
fn tenzorbus_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(create, m)?)?;
    m.add_function(wrap_pyfunction!(attach, m)?)?;
    m.add_function(wrap_pyfunction!(unlink, m)?)?;
    m.add_class::<PyRing>()?;
    m.add_class::<PyProducer>()?;
    m.add_class::<PyConsumer>()?;
    m.add_class::<PyLease>()?;
    m.add_class::<PySlotWriter>()?;
    m.add("PROTOCOL_VERSION", tenzor_core::PROTOCOL_VERSION)?;
    m.add("MAX_NDIM", tenzor_core::MAX_NDIM)?;
    m.add("MAX_CONSUMERS", tenzor_core::MAX_CONSUMERS)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
