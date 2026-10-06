//! The JS factory prepares scoped metadata; GPU creation remains on the rendering thread.
use super::*;
pub(super) fn install(
    ctx: &Ctx<'_>,
    assets: &Assets,
    samples: crate::animation::Samples,
    initial_budget: (usize, usize),
    size: [f32; 2],
    bindings: Rc<RefCell<Vec<Binding>>>,
    model_data: crate::model_data::Shared,
) -> Result<()> {
    let (initial_count, initial_bytes) = initial_budget;
    let assets = assets.clone();
    let registry = bindings;
    ctx.globals().set("__wePrepareLayer",Func::from(move|ctx:Ctx<'_>,data:String,general:String,properties:String,node:usize,time:f64|->rquickjs::Result<String>{
        let prepare=||->Result<String>{
            ensure!(data.len()<=1024*1024&&general.len()<=1024*1024&&properties.len()<=1024*1024,"dynamic layer input exceeds 1 MiB");
            ensure!(time.is_finite()&&time.abs()<=1e12,"invalid layer creation time");
            let properties:Properties=serde_json::from_str(&properties)?;
            let mut definition=crate::scene::prepare_creation(&assets,&serde_json::from_str(&data)?,&serde_json::from_str(&general)?,&properties,node,size)?;
            if let Some(id)=definition["model"]["__wallpaperd_model_data"].as_u64() {
                let info=model_data.borrow().get(id as usize)?.borrow().metadata();
                definition["__materials"]=info["materials"].clone();
                definition["__model"]=json!({"bones":[],"attachments":[],"clips":[]});
            }
            definition["__createdAt"] = json!(time);
            if definition["__sound"].is_object() {
                definition["__sound"]["started"] = json!(time);
                definition["__sound"]["__time"] = json!(time);
                definition["__sound"]["revision"] = json!(1);
                definition["__sound"]["restart"] = json!(true);
            }
            samples.append(&mut definition,node,&properties,time)?;
            let values=resolve(&definition,&properties)?;
            let mut bindings=Vec::new();collect(&definition,node,&mut Vec::new(),&mut bindings)?;
            let resolved=bindings.iter().map(|b|Ok(json!({"path":b.path,"source":b.source,"properties":resolve(&b.properties,&properties)?}))).collect::<Result<Vec<_>>>()?;
            let mut registry=registry.borrow_mut();
            ensure!(initial_count+registry.len()+bindings.len()<=512&&initial_bytes+registry.iter().chain(&bindings).map(|b|b.source.len()).sum::<usize>()<=8*1024*1024,"dynamic property script budget exceeded");
            registry.extend(bindings);
            Ok(serde_json::to_string(&json!({"definition":definition,"values":values,"bindings":resolved}))?)
        };
        prepare().map_err(|error|Exception::throw_type(&ctx,&format!("{error:#}")))
    }))?;
    let count = Cell::new(initial_count);
    let bytes = Cell::new(initial_bytes);
    ctx.globals().set(
        "__weDynamicModuleName",
        Func::from(
            move |ctx: Ctx<'_>, length: usize| -> rquickjs::Result<String> {
                if count.get() >= 512
                    || length > 1024 * 1024
                    || bytes.get().saturating_add(length) > 8 * 1024 * 1024
                {
                    return Err(Exception::throw_range(
                        &ctx,
                        "SceneScript module budget exceeded",
                    ));
                }
                let name = format!("scripts/__created_{}.js", count.get());
                count.set(count.get() + 1);
                bytes.set(bytes.get() + length);
                Ok(name)
            },
        ),
    )?;
    ctx.globals()
        .set("__weCompileCreated", Func::from(compile))?;
    Ok(())
}
fn compile<'js>(ctx: Ctx<'js>, source: String) -> rquickjs::Result<rquickjs::Object<'js>> {
    let count: Function = ctx.globals().get("__weDynamicModuleName")?;
    let name: String = count.call((source.len(),))?;
    let source = compat::lower(&source)
        .map_err(|error| Exception::throw_type(&ctx, &format!("{error:#}")))?;
    let module = Module::declare(ctx.clone(), name, source)?;
    let (module, promise) = module.eval()?;
    promise.finish::<()>()?;
    module.namespace()
}
