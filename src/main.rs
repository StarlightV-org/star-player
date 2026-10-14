use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::*;

use components::{
    media_controls::{MediaControls, MediaControlsEvent},
    seeker_bar::SeekerBar,
    volume_controls::VolumeControls,
};

mod components;
mod theme;

// A fixed timeline lets the controls be previewed without loading any media.
const PREVIEW_DURATION: f32 = 300.0;

struct PlayerShell {
    media_controls: Entity<MediaControls>,
    volume_controls: Entity<VolumeControls>,
    seeker_bar: Entity<SeekerBar>,
    _subscription: Subscription,
}

impl PlayerShell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let media_controls = cx.new(|_| MediaControls::default());
        let volume_controls = cx.new(VolumeControls::new);
        let seeker_bar = cx.new(|cx| SeekerBar::new(PREVIEW_DURATION, cx));
        let subscription = cx.subscribe_in(
            &media_controls,
            window,
            |this, _, event: &MediaControlsEvent, window, cx| {
                this.seeker_bar.update(cx, |seeker, cx| match event {
                    MediaControlsEvent::SeekBackward => seeker.seek_by(-10.0, window, cx),
                    MediaControlsEvent::SeekForward => seeker.seek_by(10.0, window, cx),
                    MediaControlsEvent::Stop => seeker.reset(window, cx),
                });
            },
        );

        Self {
            media_controls,
            volume_controls,
            seeker_bar,
            _subscription: subscription,
        }
    }
}

impl Render for PlayerShell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // .child(
            //     div()
            //         .flex()
            //         .items_center()
            //         .justify_between()
            //         .flex_shrink_0()
            //         .px_6()
            //         .py_4()
            //         .border_b_1()
            //         .border_color(cx.theme().border)
            //         .child(
            //             div()
            //                 .flex()
            //                 .items_center()
            //                 .gap_3()
            //                 .child(Icon::new(IconName::Play).text_color(cx.theme().primary))
            //                 .child(div().text_lg().child("Spacer"))
            //                 // .child(
            //                 //     div()
            //                 //         .text_xs()
            //                 //         .text_color(cx.theme().muted_foreground)
            //                 //         .child("LOCAL VIDEO PLAYER"),
            //                 // ),
            //         ),
            // )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .m_4()
                    .rounded_lg()
                    .bg(cx.theme().group_box)
                    .text_color(cx.theme().group_box_foreground)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(72.0))
                            .rounded_full()
                            .bg(cx.theme().muted)
                            .child(
                                Icon::new(IconName::Play)
                                    .size(px(28.0))
                                    .text_color(cx.theme().muted_foreground),
                            ),
                    )
                    .child(div().text_lg().child("No video loaded"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Video will appear here"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .gap_3()
                    .px_6()
                    .pt_3()
                    .pb_5()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(self.seeker_bar.clone())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(self.media_controls.clone())
                            .child(self.volume_controls.clone()),
                    ),
            )
    }
}

fn main() {
    application().with_assets(assets::Assets).run(|cx| {
        init(cx);
        theme::apply(cx);

        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Spacer".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(960.0), px(640.0)),
                cx,
            ))),
            window_min_size: Some(size(px(680.0), px(420.0))),
            ..Default::default()
        };

        open_window(options, cx, |window, cx| {
            cx.new(|cx| PlayerShell::new(window, cx))
        })
        .expect("Failed to open player window");
        cx.activate(true);
    });
}
