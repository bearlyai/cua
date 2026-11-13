use super::UiaNode;
use cua_driver_core::snapshot_store::{SnapshotPayload, SnapshotStore};
use windows::core::Interface;
use windows::Win32::UI::Accessibility::{IAccessible, IUIAutomationElement};

pub type Snapshots = SnapshotStore<UiaSnapshot>;

#[derive(Debug)]
pub struct RetainedElement {
    ptr: usize,
    pub kind: SnapshotKind,
    pub center: (i32, i32),
    pub rect: Option<(i32, i32, i32, i32)>,
    pub msaa_role: Option<i32>,
}

impl RetainedElement {
    pub fn as_ptr(&self) -> usize {
        self.ptr
    }

    pub fn is_uia(&self) -> bool {
        self.kind == SnapshotKind::Uia
    }

    pub fn focus_element(&self) -> anyhow::Result<()> {
        if !self.is_uia() {
            anyhow::bail!("element is an MSAA element, not a UIA element");
        }
        let element = unsafe { IUIAutomationElement::from_raw(self.ptr as *mut _) };
        let result = unsafe { element.SetFocus() };
        std::mem::forget(element);
        result.map_err(|e| anyhow::anyhow!("UIA SetFocus failed: {e}"))
    }

    /// The element's rectangle read live, not the one cached at read time:
    /// UIA `CurrentBoundingRectangle`, or MSAA `accLocation` for an MSAA
    /// element. MSAA has no element-not-available error, so any MSAA failure
    /// reads as unknown.
    pub(crate) fn live_bounds(&self) -> crate::tools::foreground_focus::LiveBounds {
        use crate::tools::foreground_focus::{live_bounds_from, LiveBounds};
        if self.ptr == 0 {
            return LiveBounds::Unknown;
        }
        match self.kind {
            SnapshotKind::Uia => {
                let element = unsafe { IUIAutomationElement::from_raw(self.ptr as *mut _) };
                let read = unsafe { element.CurrentBoundingRectangle() }
                    .map(|rect| (rect.left, rect.top, rect.right, rect.bottom))
                    .map_err(|error| error.code().0 as u32);
                std::mem::forget(element);
                live_bounds_from(read)
            }
            SnapshotKind::Msaa => {
                let acc = unsafe { IAccessible::from_raw(self.ptr as *mut _) };
                let (mut left, mut top, mut width, mut height) = (0i32, 0i32, 0i32, 0i32);
                let self_child = windows::core::VARIANT::from(0i32); // CHILDID_SELF
                let read = unsafe {
                    acc.accLocation(&mut left, &mut top, &mut width, &mut height, &self_child)
                };
                std::mem::forget(acc);
                match read {
                    Ok(()) => LiveBounds::Rect((left, top, left + width, top + height)),
                    Err(_) => LiveBounds::Unknown,
                }
            }
        }
    }

    pub fn element_has_keyboard_focus(&self) -> Option<bool> {
        if !self.is_uia() {
            return None;
        }
        let element = unsafe { IUIAutomationElement::from_raw(self.ptr as *mut _) };
        let focused = unsafe { element.CurrentHasKeyboardFocus() }
            .ok()
            .map(|value| value.as_bool());
        std::mem::forget(element);
        focused
    }
}

impl Clone for RetainedElement {
    fn clone(&self) -> Self {
        if self.ptr != 0 {
            unsafe {
                match self.kind {
                    SnapshotKind::Uia => {
                        let iface = IUIAutomationElement::from_raw(self.ptr as *mut _);
                        let dup = iface.clone();
                        std::mem::forget(iface);
                        std::mem::forget(dup);
                    }
                    SnapshotKind::Msaa => {
                        let iface = IAccessible::from_raw(self.ptr as *mut _);
                        let dup = iface.clone();
                        std::mem::forget(iface);
                        std::mem::forget(dup);
                    }
                }
            }
        }
        Self {
            ptr: self.ptr,
            kind: self.kind,
            center: self.center,
            rect: self.rect,
            msaa_role: self.msaa_role,
        }
    }
}

impl Drop for RetainedElement {
    fn drop(&mut self) {
        if self.ptr != 0 {
            unsafe {
                match self.kind {
                    SnapshotKind::Uia => drop(IUIAutomationElement::from_raw(self.ptr as *mut _)),
                    SnapshotKind::Msaa => drop(IAccessible::from_raw(self.ptr as *mut _)),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotKind {
    Uia,
    Msaa,
}

pub struct UiaSnapshot {
    elements: Vec<RetainedElement>,
}

impl UiaSnapshot {
    pub fn from_nodes(nodes: &[UiaNode], kind: SnapshotKind) -> Self {
        Self {
            elements: nodes
                .iter()
                .filter(|node| node.element_index.is_some())
                .map(|node| RetainedElement {
                    ptr: node.element_ptr,
                    kind,
                    center: (node.center_x, node.center_y),
                    rect: node.rect,
                    msaa_role: node.msaa_role,
                })
                .collect(),
        }
    }
}

impl SnapshotPayload for UiaSnapshot {
    type Element = RetainedElement;

    fn len(&self) -> usize {
        self.elements.len()
    }

    fn retain(&self, index: usize) -> Option<Self::Element> {
        self.elements
            .get(index)
            .filter(|element| element.ptr != 0)
            .cloned()
    }
}

#[cfg(test)]
#[path = "snapshot_uaf_repro.rs"]
mod snapshot_uaf_repro;
