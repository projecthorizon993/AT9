use log::{debug, error, info, warn};
use parking_lot::Mutex;
use std::sync::Arc;
use windows::Win32::Foundation::HWND;
use windows::core::HRESULT;

/// Vulkan rendering context: instance, device, swapchain, and resources.
pub struct VulkanContext {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub surface_loader: ash::extensions::ext::DebugUtils,
    pub debug_messenger: Option<ash::vk::DebugUtilsMessengerEXT>,
    pub surface: ash::vk::SurfaceKHR,
    pub surface_loader_khr: ash::extensions::khr::Surface,
    pub swapchain_loader: ash::extensions::khr::Swapchain,
    pub swapchain: ash::vk::SwapchainKHR,
    pub physical_device: ash::vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: ash::vk::Queue,
    pub queue_family_index: u32,
    pub swapchain_images: Vec<ash::vk::Image>,
    pub swapchain_image_views: Vec<ash::vk::ImageView>,
    pub render_pass: ash::vk::RenderPass,
    pub pipeline_cache: ash::vk::PipelineCache,
    pub framebuffers: Vec<ash::vk::Framebuffer>,
    pub command_pool: ash::vk::CommandPool,
    pub command_buffers: Vec<ash::vk::CommandBuffer>,
    pub image_available_semaphore: ash::vk::Semaphore,
    pub render_finished_semaphore: ash::vk::Semaphore,
    pub in_flight_fence: ash::vk::Fence,
    pub current_frame: usize,
    pub swapchain_format: ash::vk::Format,
    pub swapchain_extent: ash::vk::Extent2D,
    pub width: u32,
    pub height: u32,
    pub hwnd: HWND,
    /// Damage tracking: whether the current frame needs re-recording.
    pub dirty: Mutex<bool>,
}

impl VulkanContext {
    /// Create a Vulkan context for the given window handle.
    pub unsafe fn new(hwnd: HWND, width: u32, height: u32) -> anyhow::Result<Self> {
        let entry = ash::Entry::linked();

        // Instance with validation layers in debug.
        let app_name =
            windows::core::PCWSTR::from_raw("Seelen Compositor\0".encode_utf16().as_ptr());
        let _ = app_name;

        let app_name_c = std::ffi::CString::new("Seelen Compositor")?;
        let engine_name_c = std::ffi::CString::new("Seelen")?;

        let app_info = ash::vk::ApplicationInfo::builder()
            .application_name(app_name_c.as_c_str())
            .application_version(ash::vk::make_api_version(0, 2, 8, 11))
            .engine_name(engine_name_c.as_c_str())
            .engine_version(ash::vk::make_api_version(0, 2, 8, 11))
            .api_version(ash::vk::API_VERSION_1_3);

        let mut extension_names = vec![
            ash::extensions::khr::Surface::name().as_ptr(),
            ash::extensions::khr::Win32Surface::name().as_ptr(),
        ];

        let mut layer_names: Vec<*const i8> = Vec::new();

        #[cfg(debug_assertions)]
        {
            extension_names.push(ash::extensions::ext::DebugUtils::name().as_ptr());
            layer_names.push(ash::extensions::ext::DebugUtils::name().as_ptr());
        }

        let create_info = ash::vk::InstanceCreateInfo::builder()
            .application_info(&app_info)
            .enabled_extension_names(&extension_names)
            .enabled_layer_names(&layer_names);

        let instance = entry.create_instance(&create_info, None)?;

        // Debug messenger (debug only).
        let debug_messenger = if cfg!(debug_assertions) {
            let debug_create_info = ash::vk::DebugUtilsMessengerCreateInfoEXT::builder()
                .message_severity(
                    ash::vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                        | ash::vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                )
                .message_type(
                    ash::vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | ash::vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | ash::vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(vulkan_debug_callback));
            Some(instance.create_debug_utils_messenger_ext(&debug_create_info, None)?)
        } else {
            None
        };

        // Surface (Win32).
        let surface_loader = ash::extensions::ext::DebugUtils::new(&entry, &instance);
        let win32_surface_create_info = ash::vk::Win32SurfaceCreateInfoKHR::builder()
            .hinstance(unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None)? })
            .hwnd(hwnd);
        let surface_loader_khr = ash::extensions::khr::Surface::new(&entry, &instance);
        let surface =
            surface_loader_khr.create_win32_surface_khr(&win32_surface_create_info, None)?;

        // Physical device selection: prefer integrated GPU for power efficiency.
        let physical_devices = instance.enumerate_physical_devices()?;
        if physical_devices.is_empty() {
            return Err(anyhow::anyhow!("No Vulkan-capable GPU found"));
        }

        let mut selected = None;
        for pd in &physical_devices {
            let props = instance.get_physical_device_properties(*pd);
            let device_type = props.device_type;
            if device_type == ash::vk::PhysicalDeviceType::INTEGRATED_GPU {
                selected = Some(*pd);
                break;
            }
        }
        let physical_device = selected.unwrap_or(physical_devices[0]);

        let props = instance.get_physical_device_properties(physical_device);
        info!(
            "Using Vulkan device: {}",
            String::from_utf8_lossy(
                &props.device_name[..props
                    .device_name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(props.device_name.len())]
            )
        );

        // Queue family with present support.
        let queue_family_props =
            instance.get_physical_device_queue_family_properties(physical_device);
        let mut queue_family_index = None;
        for (i, props) in queue_family_props.iter().enumerate() {
            let supports_graphics = props.queue_flags.contains(ash::vk::QueueFlags::GRAPHICS);
            let supports_present = unsafe {
                surface_loader_khr.get_physical_device_surface_support_khr(
                    physical_device,
                    i as u32,
                    surface,
                )
            }
            .unwrap_or(false);
            if supports_graphics && supports_present {
                queue_family_index = Some(i as u32);
                break;
            }
        }
        let queue_family_index = queue_family_index
            .ok_or_else(|| anyhow::anyhow!("No queue family supports graphics + present"))?;

        // Logical device.
        let queue_create_infos = [ash::vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .queue_priorities(&[1.0])
            .build()];

        let device_extensions = [ash::extensions::khr::Swapchain::name().as_ptr()];

        let device_create_info = ash::vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions);

        let device = instance.create_device(physical_device, &device_create_info, None)?;
        let queue = device.get_device_queue(queue_family_index, 0);

        // Swapchain.
        let swapchain_loader = ash::extensions::khr::Swapchain::new(&instance, &device);

        let caps = surface_loader_khr
            .get_physical_device_surface_capabilities_khr(physical_device, surface)?;
        let formats =
            surface_loader_khr.get_physical_device_surface_formats_khr(physical_device, surface)?;
        let present_modes = surface_loader_khr
            .get_physical_device_surface_present_modes_khr(physical_device, surface)?;

        // Choose format: BGRA8 unorm with SRGB or UNORM.
        let surface_format = formats
            .iter()
            .find(|f| f.format == ash::vk::Format::B8G8R8A8_UNORM)
            .unwrap_or(&formats[0]);

        let swapchain_format = surface_format.format;
        let swapchain_extent = if caps.current_extent.width == u32::MAX {
            ash::vk::Extent2D::builder()
                .width(width)
                .height(height)
                .build()
        } else {
            caps.current_extent
        };

        let mut image_count = caps.min_image_count + 1;
        if caps.max_image_count > 0 && image_count > caps.max_image_count {
            image_count = caps.max_image_count;
        }
        // Target 2 images for low VRAM.
        image_count = image_count.min(3).max(2);

        let pre_transform = if caps
            .supported_transforms
            .contains(ash::vk::SurfaceTransformFlagsKHR::IDENTITY)
        {
            ash::vk::SurfaceTransformFlagsKHR::IDENTITY
        } else {
            caps.current_transform
        };

        let composite_alpha = ash::vk::CompositeAlphaFlagsKHR::OPAQUE;

        let present_mode = present_modes
            .iter()
            .cloned()
            .find(|&m| m == ash::vk::PresentModeKHR::FIFO)
            .unwrap_or(ash::vk::PresentModeKHR::FIFO);

        let swapchain_create_info = ash::vk::SwapchainCreateInfoKHR::builder()
            .surface(surface)
            .min_image_count(image_count)
            .image_format(swapchain_format)
            .image_color_space(surface_format.color_space)
            .image_extent(swapchain_extent)
            .image_array_layers(1)
            .image_usage(
                ash::vk::ImageUsageFlags::COLOR_ATTACHMENT | ash::vk::ImageUsageFlags::TRANSFER_DST,
            )
            .image_sharing_mode(ash::vk::SharingMode::EXCLUSIVE)
            .pre_transform(pre_transform)
            .composite_alpha(composite_alpha)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(ash::vk::SwapchainKHR::null());

        let swapchain = swapchain_loader.create_swapchain_khr(&swapchain_create_info, None)?;
        let swapchain_images = swapchain_loader.get_swapchain_images_khr(swapchain)?;

        // Image views.
        let swapchain_image_views: Vec<ash::vk::ImageView> = swapchain_images
            .iter()
            .map(|&img| {
                let create_info = ash::vk::ImageViewCreateInfo::builder()
                    .image(img)
                    .view_type(ash::vk::ImageViewType::TYPE_2D)
                    .format(swapchain_format)
                    .components(ash::vk::ComponentMapping {
                        r: ash::vk::ComponentSwizzle::IDENTITY,
                        g: ash::vk::ComponentSwizzle::IDENTITY,
                        b: ash::vk::ComponentSwizzle::IDENTITY,
                        a: ash::vk::ComponentSwizzle::IDENTITY,
                    })
                    .subresource_range(ash::vk::ImageSubresourceRange {
                        aspect_mask: ash::vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    });
                unsafe { device.create_image_view(&create_info, None) }.unwrap()
            })
            .collect();

        // Render pass.
        let attachment_descs = [ash::vk::AttachmentDescription::builder()
            .format(swapchain_format)
            .samples(ash::vk::SampleCountFlags::TYPE_1)
            .load_op(ash::vk::AttachmentLoadOp::CLEAR)
            .store_op(ash::vk::AttachmentStoreOp::STORE)
            .stencil_load_op(ash::vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(ash::vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(ash::vk::ImageLayout::UNDEFINED)
            .final_layout(ash::vk::ImageLayout::PRESENT_SRC_KHR)
            .build()];

        let color_attachment_refs = [ash::vk::AttachmentReference::builder()
            .attachment(0)
            .layout(ash::vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .build()];

        let subpasses = [ash::vk::SubpassDescription::builder()
            .pipeline_bind_point(ash::vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_attachment_refs)
            .build()];

        let render_pass_create_info = ash::vk::RenderPassCreateInfo::builder()
            .attachments(&attachment_descs)
            .subpasses(&subpasses);

        let render_pass = device.create_render_pass(&render_pass_create_info, None)?;

        // Framebuffers.
        let framebuffers: Vec<ash::vk::Framebuffer> = swapchain_image_views
            .iter()
            .map(|&iv| {
                let attachments = [iv];
                let create_info = ash::vk::FramebufferCreateInfo::builder()
                    .render_pass(render_pass)
                    .attachments(&attachments)
                    .width(swapchain_extent.width)
                    .height(swapchain_extent.height)
                    .layers(1);
                unsafe { device.create_framebuffer(&create_info, None) }.unwrap()
            })
            .collect();

        // Command pool.
        let command_pool_create_info = ash::vk::CommandPoolCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .flags(ash::vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);

        let command_pool = device.create_command_pool(&command_pool_create_info, None)?;

        // Command buffers.
        let cmd_buf_alloc_info = ash::vk::CommandBufferAllocateInfo::builder()
            .command_pool(command_pool)
            .level(ash::vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(swapchain_images.len() as u32);

        let command_buffers = device.allocate_command_buffers(&cmd_buf_alloc_info)?;

        // Sync primitives.
        let semaphore_create_info = ash::vk::SemaphoreCreateInfo::builder();
        let fence_create_info =
            ash::vk::FenceCreateInfo::builder().flags(ash::vk::FenceCreateFlags::SIGNALED);

        let image_available_semaphore = device.create_semaphore(&semaphore_create_info, None)?;
        let render_finished_semaphore = device.create_semaphore(&semaphore_create_info, None)?;
        let in_flight_fence = device.create_fence(&fence_create_info, None)?;

        let pipeline_cache = device
            .create_pipeline_cache(&ash::vk::PipelineCacheCreateInfo::builder().build(), None)?;

        Ok(Self {
            entry,
            instance,
            surface_loader,
            debug_messenger,
            surface,
            surface_loader_khr,
            swapchain_loader,
            swapchain,
            physical_device,
            device,
            queue,
            queue_family_index,
            swapchain_images,
            swapchain_image_views,
            render_pass,
            pipeline_cache,
            framebuffers,
            command_pool,
            command_buffers,
            image_available_semaphore,
            render_finished_semaphore,
            in_flight_fence,
            current_frame: 0,
            swapchain_format,
            swapchain_extent,
            width,
            height,
            hwnd,
            dirty: Mutex::new(true),
        })
    }

    /// Mark the context as dirty (needs re-recording on next frame).
    pub fn mark_dirty(&self) {
        *self.dirty.lock() = true;
    }

    /// Whether the context is dirty.
    pub fn is_dirty(&self) -> bool {
        *self.dirty.lock()
    }

    /// Clear the dirty flag.
    pub fn clear_dirty(&self) {
        *self.dirty.lock() = false;
    }

    /// Begin a frame: acquire next swapchain image.
    pub unsafe fn begin_frame(&mut self) -> anyhow::Result<ash::vk::Image> {
        self.device
            .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)?;

        let (image_index, _suboptimal) = self.swapchain_loader.acquire_next_image_khr(
            self.swapchain,
            u64::MAX,
            self.image_available_semaphore,
            ash::vk::Fence::null(),
        )?;

        self.device.reset_fences(&[self.in_flight_fence])?;
        Ok(image_index)
    }

    /// Record and submit a draw command buffer for the given image index.
    ///
    /// In MVP we clear to a transparent color. Subclasses/callers fill in
    /// actual UI drawing via `record_ui_commands`.
    pub unsafe fn draw_frame(
        &mut self,
        image_index: u32,
        clear_color: ash::vk::ClearColorValue,
    ) -> anyhow::Result<()> {
        let command_buffer = self.command_buffers[image_index as usize];

        // Reset command buffer.
        self.device
            .reset_command_buffer(command_buffer, ash::vk::CommandBufferResetFlags::empty())?;

        let begin_info = ash::vk::CommandBufferBeginInfo::builder()
            .flags(ash::vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        self.device
            .begin_command_buffer(command_buffer, &begin_info)?;

        // Begin render pass.
        let clear_values = [ash::vk::ClearValue { color: clear_color }];

        let render_pass_begin_info = ash::vk::RenderPassBeginInfo::builder()
            .render_pass(self.render_pass)
            .framebuffer(self.framebuffers[image_index as usize])
            .render_area(ash::vk::Rect2D {
                offset: ash::vk::Offset2D { x: 0, y: 0 },
                extent: self.swapchain_extent,
            })
            .clear_values(&clear_values);

        self.device.cmd_begin_render_pass(
            command_buffer,
            &render_pass_begin_info,
            ash::vk::SubpassContents::INLINE,
        );

        // TODO: Draw UI elements here via a pipeline bound to this command buffer.
        // For MVP, the render pass is empty (clear only).

        self.device.cmd_end_render_pass(command_buffer);

        self.device.end_command_buffer(command_buffer)?;

        // Submit.
        let wait_semaphores = [self.image_available_semaphore];
        let wait_stages = [ash::vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let signal_semaphores = [self.render_finished_semaphore];

        let submit_info = [ash::vk::SubmitInfo::builder()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&[command_buffer])
            .signal_semaphores(&signal_semaphores)
            .build()];

        self.device
            .queue_submit(self.queue, &submit_info, self.in_flight_fence)?;

        // Present.
        let swapchains = [self.swapchain];
        let image_indices = [image_index];
        let present_info = ash::vk::PresentInfoKHR::builder()
            .wait_semaphores(&signal_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);

        self.swapchain_loader.queue_present_khr(&present_info)?;

        self.current_frame = (self.current_frame + 1) % self.swapchain_images.len();
        self.clear_dirty();
        Ok(())
    }

    /// Recreate the swapchain (e.g. after window resize).
    pub unsafe fn recreate_swapchain(&mut self, width: u32, height: u32) -> anyhow::Result<()> {
        self.device.wait_idle()?;

        // Destroy old swapchain resources.
        for fb in self.framebuffers.iter() {
            self.device.destroy_framebuffer(*fb, None);
        }
        for iv in self.swapchain_image_views.iter() {
            self.device.destroy_image_view(*iv, None);
        }
        self.swapchain_loader
            .destroy_swapchain_khr(self.swapchain, None);

        self.width = width;
        self.height = height;
        self.swapchain_extent = ash::vk::Extent2D::builder()
            .width(width)
            .height(height)
            .build();

        // Recreate swapchain (same params as initial creation).
        // Simplified: in a full implementation we'd re-query surface caps.
        let caps = self
            .surface_loader_khr
            .get_physical_device_surface_capabilities_khr(self.physical_device, self.surface)?;
        let formats = self
            .surface_loader_khr
            .get_physical_device_surface_formats_khr(self.physical_device, self.surface)?;
        let surface_format = formats
            .iter()
            .find(|f| f.format == ash::vk::Format::B8G8R8A8_UNORM)
            .unwrap_or(&formats[0]);

        let swapchain_create_info = ash::vk::SwapchainCreateInfoKHR::builder()
            .surface(self.surface)
            .min_image_count(caps.min_image_count.max(2))
            .image_format(surface_format.format)
            .image_color_space(surface_format.color_space)
            .image_extent(self.swapchain_extent)
            .image_array_layers(1)
            .image_usage(
                ash::vk::ImageUsageFlags::COLOR_ATTACHMENT | ash::vk::ImageUsageFlags::TRANSFER_DST,
            )
            .image_sharing_mode(ash::vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(ash::vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(ash::vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(ash::vk::SwapchainKHR::null());

        self.swapchain = self
            .swapchain_loader
            .create_swapchain_khr(&swapchain_create_info, None)?;
        self.swapchain_images = self
            .swapchain_loader
            .get_swapchain_images_khr(self.swapchain)?;
        self.swapchain_image_views = self
            .swapchain_images
            .iter()
            .map(|&img| {
                let create_info = ash::vk::ImageViewCreateInfo::builder()
                    .image(img)
                    .view_type(ash::vk::ImageViewType::TYPE_2D)
                    .format(surface_format.format)
                    .components(ash::vk::ComponentMapping {
                        r: ash::vk::ComponentSwizzle::IDENTITY,
                        g: ash::vk::ComponentSwizzle::IDENTITY,
                        b: ash::vk::ComponentSwizzle::IDENTITY,
                        a: ash::vk::ComponentSwizzle::IDENTITY,
                    })
                    .subresource_range(ash::vk::ImageSubresourceRange {
                        aspect_mask: ash::vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    });
                unsafe { self.device.create_image_view(&create_info, None) }.unwrap()
            })
            .collect();

        self.framebuffers = self
            .swapchain_image_views
            .iter()
            .map(|&iv| {
                let attachments = [iv];
                let create_info = ash::vk::FramebufferCreateInfo::builder()
                    .render_pass(self.render_pass)
                    .attachments(&attachments)
                    .width(self.swapchain_extent.width)
                    .height(self.swapchain_extent.height)
                    .layers(1);
                unsafe { self.device.create_framebuffer(&create_info, None) }.unwrap()
            })
            .collect();

        self.command_buffers = self.device.allocate_command_buffers(
            &ash::vk::CommandBufferAllocateInfo::builder()
                .command_pool(self.command_pool)
                .level(ash::vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(self.swapchain_images.len() as u32),
        )?;

        self.mark_dirty();
        Ok(())
    }
}

impl Drop for VulkanContext {
    fn drop(&mut self) {
        unsafe {
            self.device.wait_idle().ok();
            for fb in self.framebuffers.iter() {
                self.device.destroy_framebuffer(*fb, None);
            }
            for iv in self.swapchain_image_views.iter() {
                self.device.destroy_image_view(*iv, None);
            }
            self.swapchain_loader
                .destroy_swapchain_khr(self.swapchain, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_fence(self.in_flight_fence, None);
            self.device
                .destroy_semaphore(self.render_finished_semaphore, None);
            self.device
                .destroy_semaphore(self.image_available_semaphore, None);
            self.device
                .destroy_pipeline_cache(self.pipeline_cache, None);
            self.device.destroy_render_pass(self.render_pass, None);
            self.device.destroy_device(None);
            if let Some(dm) = self.debug_messenger {
                self.surface_loader
                    .destroy_debug_utils_messenger_ext(dm, None);
            }
            self.surface_loader_khr
                .destroy_surface_khr(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

/// Vulkan debug callback for validation layers.
extern "system" fn vulkan_debug_callback(
    message_severity: ash::vk::DebugUtilsMessageSeverityFlagsEXT,
    message_type: ash::vk::DebugUtilsMessageTypeFlagsEXT,
    p_callback_data: *const ash::vk::DebugUtilsMessengerCallbackDataEXT,
    _user_data: *mut std::ffi::c_void,
) -> ash::vk::Bool32 {
    let callback_data = unsafe { *p_callback_data };
    let message = unsafe {
        let c_str = std::ffi::CStr::from_ptr(callback_data.p_message);
        c_str.to_string_lossy().into_owned()
    };

    match message_severity {
        ash::vk::DebugUtilsMessageSeverityFlagsEXT::ERROR => {
            error!("[Vulkan Validation ERROR] {:?}: {}", message_type, message)
        }
        ash::vk::DebugUtilsMessageSeverityFlagsEXT::WARNING => {
            warn!(
                "[Vulkan Validation WARNING] {:?}: {}",
                message_type, message
            )
        }
        _ => {
            debug!("[Vulkan Validation] {:?}: {}", message_type, message)
        }
    }

    ash::vk::FALSE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clear_color_default() {
        let clear = ash::vk::ClearColorValue {
            float32: [0.0, 0.0, 0.0, 1.0],
        };
        assert_eq!(clear.float32[3], 1.0);
    }
}
