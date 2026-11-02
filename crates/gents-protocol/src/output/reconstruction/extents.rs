use super::{
    closing_for_reference, extent_streams_from_closing, DependencyDenial, ObservedSegment,
    ReconstructedStream,
};
use crate::output::{OutputSource, PayloadRef, ReconstructionError, StreamPayload};
use rapidhash::{HashMapExt, RapidHashMap};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

pub(super) struct SealedExtent {
    pub(super) streams: Vec<ReconstructedStream>,
    pub(super) signatures: RapidHashMap<(u32, u32), Vec<usize>>,
}

impl SealedExtent {
    fn new(streams: Vec<ReconstructedStream>) -> Self {
        let mut signatures: RapidHashMap<(u32, u32), Vec<usize>> = RapidHashMap::new();
        for (index, stream) in streams.iter().enumerate() {
            if matches!(
                stream.declaration.payload,
                StreamPayload::ReasoningSignature
            ) {
                signatures
                    .entry((
                        stream.declaration.block_index,
                        stream.declaration.part_index,
                    ))
                    .or_default()
                    .push(index);
            }
        }
        Self {
            streams,
            signatures,
        }
    }
}

struct ObservationIndex<'a> {
    by_doc: RapidHashMap<&'a str, Vec<ObservedSegment<'a>>>,
    by_source: RapidHashMap<(&'a str, &'a OutputSource), Vec<ObservedSegment<'a>>>,
}

impl<'a> ObservationIndex<'a> {
    fn new(records: &[ObservedSegment<'a>]) -> Self {
        let mut by_doc: RapidHashMap<&'a str, Vec<ObservedSegment<'a>>> = RapidHashMap::new();
        let mut by_source: RapidHashMap<(&'a str, &'a OutputSource), Vec<_>> = RapidHashMap::new();
        for record in records {
            by_doc.entry(record.doc_id).or_default().push(*record);
            by_source
                .entry((
                    record.segment.request_doc_id.as_str(),
                    &record.segment.source,
                ))
                .or_default()
                .push(*record);
        }
        Self { by_doc, by_source }
    }
}

pub(super) struct Extents<'s, 'a> {
    records: &'s [ObservedSegment<'a>],
    denied: &'s [String],
    dependency_denials: &'s [DependencyDenial],
    #[cfg(test)]
    reuse: bool,
    index: OnceCell<ObservationIndex<'a>>,
    closings: RefCell<RapidHashMap<&'a str, ObservedSegment<'a>>>,
    extents: RefCell<RapidHashMap<&'a str, Rc<SealedExtent>>>,
    #[cfg(test)]
    closing_scans: std::cell::Cell<usize>,
    #[cfg(test)]
    extent_builds: std::cell::Cell<usize>,
}

impl<'s, 'a> Extents<'s, 'a> {
    pub(super) fn new(
        records: &'s [ObservedSegment<'a>],
        denied: &'s [String],
        dependency_denials: &'s [DependencyDenial],
    ) -> Self {
        Self {
            records,
            denied,
            dependency_denials,
            #[cfg(test)]
            reuse: true,
            index: OnceCell::new(),
            closings: RefCell::default(),
            extents: RefCell::default(),
            #[cfg(test)]
            closing_scans: Default::default(),
            #[cfg(test)]
            extent_builds: Default::default(),
        }
    }

    fn reuses(&self) -> bool {
        #[cfg(test)]
        return self.reuse;
        #[cfg(not(test))]
        true
    }

    fn index(&self) -> &ObservationIndex<'a> {
        self.index
            .get_or_init(|| ObservationIndex::new(self.records))
    }

    pub(super) fn closing(
        &self,
        reference: &PayloadRef,
    ) -> Result<ObservedSegment<'a>, ReconstructionError> {
        if self.reuses() {
            if let Some(closing) = self.closings.borrow().get(reference.close_doc_id.as_str()) {
                return Ok(*closing);
            }
        }
        #[cfg(test)]
        self.closing_scans.set(self.closing_scans.get() + 1);
        let candidates = if self.reuses() {
            self.index()
                .by_doc
                .get(reference.close_doc_id.as_str())
                .map_or(&[][..], Vec::as_slice)
        } else {
            self.records
        };
        let closing =
            closing_for_reference(candidates, self.denied, self.dependency_denials, reference)?;
        if self.reuses() {
            self.closings.borrow_mut().insert(closing.doc_id, closing);
        }
        Ok(closing)
    }

    /// Equal to `reconstruct_extent_streams(.., reference)`, with `reference.stream`
    /// guaranteed in bounds on success.
    pub(super) fn extent(
        &self,
        reference: &PayloadRef,
    ) -> Result<Rc<SealedExtent>, ReconstructionError> {
        let reused = if self.reuses() {
            self.extents
                .borrow()
                .get(reference.close_doc_id.as_str())
                .cloned()
        } else {
            None
        };
        let extent = match reused {
            Some(extent) => extent,
            None => {
                let closing = self.closing(reference)?;
                #[cfg(test)]
                self.extent_builds.set(self.extent_builds.get() + 1);
                let same_source = if self.reuses() {
                    self.index()
                        .by_source
                        .get(&(
                            closing.segment.request_doc_id.as_str(),
                            &closing.segment.source,
                        ))
                        .map_or(&[][..], Vec::as_slice)
                } else {
                    self.records
                };
                let extent = Rc::new(SealedExtent::new(extent_streams_from_closing(
                    same_source,
                    self.denied,
                    closing,
                    reference,
                )?));
                if self.reuses() {
                    self.extents
                        .borrow_mut()
                        .insert(closing.doc_id, Rc::clone(&extent));
                }
                extent
            }
        };
        if (reference.stream as usize) < extent.streams.len() {
            Ok(extent)
        } else {
            Err(ReconstructionError::InvalidReference {
                reference: reference.clone(),
            })
        }
    }

    /// Equal to `reconstruct_stream(.., reference)`.
    pub(super) fn stream(
        &self,
        reference: &PayloadRef,
    ) -> Result<ReconstructedStream, ReconstructionError> {
        Ok(self.extent(reference)?.streams[reference.stream as usize].clone())
    }
}
