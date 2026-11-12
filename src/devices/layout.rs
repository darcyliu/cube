//! Reserved dynamic MMIO aperture, disjoint from fixed platform devices.
//! Keep disk zero at its firmware-compatible address; move only extra disks.
//! Balloon, vsock, and GPU consume the same allocator after disks and shares,
//! so turning them on does not move an existing disk or share.

const APERTURE_BASE: u64 = 0x0a02_0000;
const APERTURE_STRIDE: u64 = 0x200;
const FIRST_IRQ: u32 = 48;
const MAX_OPTIONAL: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceRole {
    ExtraDisk,
    Share,
    Balloon,
    Vsock,
    Gpu,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeviceRequest {
    pub extra_disks: usize,
    pub shares: usize,
    pub balloon: bool,
    pub vsock: bool,
    pub gpu: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedDevice {
    pub base: u64,
    pub irq: u32,
    pub role: DeviceRole,
}

#[derive(Clone, Debug, Default)]
pub struct DevicePlan {
    pub devices: Vec<PlacedDevice>,
}

impl DevicePlan {
    pub fn of(&self, role: DeviceRole) -> Vec<(u64, u32)> {
        self.devices
            .iter()
            .filter(|d| d.role == role)
            .map(|d| (d.base, d.irq))
            .collect()
    }

    pub fn one(&self, role: DeviceRole) -> Option<(u64, u32)> {
        let found: Vec<_> = self.of(role);
        match found.len() {
            0 => None,
            1 => Some(found[0]),
            _ => None,
        }
    }
}

pub fn plan_devices(request: DeviceRequest) -> Result<DevicePlan, String> {
    let mut roles = Vec::new();
    roles.extend(std::iter::repeat_n(
        DeviceRole::ExtraDisk,
        request.extra_disks,
    ));
    roles.extend(std::iter::repeat_n(DeviceRole::Share, request.shares));
    if request.balloon {
        roles.push(DeviceRole::Balloon);
    }
    if request.vsock {
        roles.push(DeviceRole::Vsock);
    }
    if request.gpu {
        roles.push(DeviceRole::Gpu);
    }
    if roles.len() > MAX_OPTIONAL {
        return Err("at most 64 additional disks and optional virtio devices are supported".into());
    }
    let mut devices = Vec::with_capacity(roles.len());
    for (i, role) in roles.into_iter().enumerate() {
        let base = APERTURE_BASE + i as u64 * APERTURE_STRIDE;
        let irq = FIRST_IRQ + i as u32;
        let end = base + APERTURE_STRIDE;
        if end <= base || irq < FIRST_IRQ {
            return Err("device layout overflow".into());
        }
        devices.push(PlacedDevice { base, irq, role });
    }
    let plan = DevicePlan { devices };
    validate_plan(&plan)?;
    Ok(plan)
}

fn validate_plan(plan: &DevicePlan) -> Result<(), String> {
    for (i, dev) in plan.devices.iter().enumerate() {
        if dev.base < APERTURE_BASE || dev.irq < FIRST_IRQ {
            return Err(format!(
                "device slot {i} collides with a fixed platform device"
            ));
        }
        for other in &plan.devices[..i] {
            let overlap =
                dev.base < other.base + APERTURE_STRIDE && other.base < dev.base + APERTURE_STRIDE;
            if overlap || dev.irq == other.irq {
                return Err(format!("device slot {i} overlaps another optional device"));
            }
        }
    }
    Ok(())
}

pub fn dynamic_devices(disks: usize, shares: usize) -> Result<Vec<(u64, u32)>, String> {
    let plan = plan_devices(DeviceRequest {
        extra_disks: disks.saturating_sub(1),
        shares,
        ..DeviceRequest::default()
    })?;
    Ok(plan.devices.iter().map(|d| (d.base, d.irq)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_ranges_avoid_graphics_and_bound_interrupts() {
        let slots = dynamic_devices(4, 3).unwrap();
        assert_eq!(slots.len(), 6);
        for (i, &(base, irq)) in slots.iter().enumerate() {
            assert!(base >= 0x0a02_0000 && irq > 41);
            for &(other, other_irq) in &slots[..i] {
                assert!(base >= other + 0x200);
                assert_ne!(irq, other_irq);
            }
        }
        assert!(dynamic_devices(65, 1).is_err());
    }

    #[test]
    fn optional_devices_follow_disks_and_shares_without_moving_them() {
        let disks_only = plan_devices(DeviceRequest {
            extra_disks: 2,
            shares: 1,
            ..DeviceRequest::default()
        })
        .unwrap();
        let full = plan_devices(DeviceRequest {
            extra_disks: 2,
            shares: 1,
            balloon: true,
            vsock: true,
            gpu: true,
        })
        .unwrap();
        assert_eq!(
            full.of(DeviceRole::ExtraDisk),
            disks_only.of(DeviceRole::ExtraDisk)
        );
        assert_eq!(full.of(DeviceRole::Share), disks_only.of(DeviceRole::Share));
        let balloon = full.one(DeviceRole::Balloon).unwrap();
        let vsock = full.one(DeviceRole::Vsock).unwrap();
        let gpu = full.one(DeviceRole::Gpu).unwrap();
        assert!(balloon.0 > disks_only.devices.last().unwrap().base);
        assert!(vsock.0 > balloon.0 && gpu.0 > vsock.0);
        assert!(gpu.0 >= 0x0a02_0000 + 4 * 0x200);
        assert!(full.one(DeviceRole::ExtraDisk).is_none());
        assert!(
            plan_devices(DeviceRequest {
                extra_disks: 64,
                shares: 1,
                balloon: true,
                ..DeviceRequest::default()
            })
            .is_err()
        );
    }
}
