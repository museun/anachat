use std::sync::{Arc, Mutex};

use anathema::{
    core::{
        contexts::{PaintCtx, PositionCtx, WithSize},
        error::Result,
        AnyWidget, FactoryContext, LayoutNodes, LocalPos, Nodes, WidgetFactory, WidgetStyle,
    },
    render::Size,
    values::{Context, NodeId, Value},
    widgets::layout::text::{TextLayout, Wrap},
};

use crate::geometry::{pos2, Pos2, Rect};

#[derive(Debug)]
pub struct Tab {
    text: Value<String>,
    style: WidgetStyle,
    layout: TextLayout,
}

impl Tab {
    const KIND: &'static str = "Tab";
}

impl anathema::core::Widget for Tab {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn update(&mut self, context: &Context<'_, '_>, node_id: &NodeId) {
        self.text.resolve(context, node_id);
        self.style.resolve(context, node_id);
    }

    fn layout(&mut self, nodes: &mut LayoutNodes<'_, '_, '_>) -> Result<Size> {
        let constraints = nodes.constraints;
        self.layout.reset(
            Size::new(constraints.max_width, constraints.max_height),
            true,
        );
        self.layout.process(self.text.str());
        self.layout.finish();

        let size = self.layout.size();
        Ok(size)
    }

    fn paint(&mut self, children: &mut Nodes<'_>, mut ctx: PaintCtx<'_, WithSize>) {
        let start = ctx.global_pos;
        if let Some(LocalPos { x, y }) =
            ctx.print(self.text.str(), self.style.style(), LocalPos::ZERO)
        {
            TabRegions::insert(
                self.text.str(),
                Rect::from_min_max(
                    pos2(start.x as _, start.y as _),
                    pos2(start.x as u16 + x as u16, start.y as u16 + y as u16),
                ),
            );
        }

        for (widget, children) in children.iter_mut() {
            let ctx = ctx.to_unsized();
            widget.paint(children, ctx);
        }
    }

    fn position(&mut self, _children: &mut Nodes<'_>, _ctx: PositionCtx) {}
}

pub struct TabFactory;

impl WidgetFactory for TabFactory {
    fn make(&self, mut ctx: FactoryContext<'_>) -> Result<Box<dyn AnyWidget>> {
        let widget = Tab {
            style: ctx.style(),
            layout: TextLayout::new(Size::ZERO, false, Wrap::Normal),
            text: ctx.text.take(),
        };

        Ok(Box::new(widget))
    }
}

#[derive(Default)]
pub struct TabRegions {
    map: Vec<(Rect, Arc<String>)>,
}

static REGIONS: Mutex<TabRegions> = Mutex::new(TabRegions { map: Vec::new() });

impl TabRegions {
    pub fn insert(name: &str, rect: Rect) {
        let g = &mut *REGIONS.lock().unwrap();
        if let Some(pos) = g.map.iter().position(|(_, v)| &**v == name) {
            g.map[pos].0 = rect;
        } else {
            g.map.push((rect, Arc::new(name.to_string())))
        }
    }

    pub fn get_all() -> Vec<(Rect, Arc<String>)> {
        let g = &*REGIONS.lock().unwrap();
        g.map.clone()
    }

    pub fn containing_point(pos: Pos2) -> Option<Arc<String>> {
        let g = &*REGIONS.lock().unwrap();
        g.map
            .iter()
            .find_map(|(k, v)| k.contains(pos).then(|| Arc::clone(&v)))
    }

    pub fn get(rect: Rect) -> Option<Arc<String>> {
        let g = &*REGIONS.lock().unwrap();
        g.map
            .iter()
            .find_map(|(k, v)| (*k == rect).then(|| Arc::clone(&v)))
    }
}
