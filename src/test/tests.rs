#![allow(non_local_definitions)]

use {
    super::{Ctx, SEED, Tester},
    anyhow::{Error, Result, anyhow},
    exports::componentize_py::test::streams_and_futures,
    futures::FutureExt,
    once_cell::sync::Lazy,
    std::{
        mem,
        ops::DerefMut,
        pin::Pin,
        str,
        sync::{Arc, Mutex},
        task::{self, Context, Poll},
        time::Duration,
    },
    wasmtime::{
        Store, StoreContextMut,
        component::{
            Accessor, Destination, FutureConsumer, FutureProducer, FutureReader, HasSelf,
            InstancePre, Lift, Linker, Resource, ResourceAny, Source, StreamConsumer,
            StreamProducer, StreamReader, StreamResult, VecBuffer,
        },
    },
    wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder, WasiView},
};

wasmtime::component::bindgen!({
    path: "src/test/wit",
    world: "tests",
    imports: { default: async | trappable },
    exports: { default: async | task_exit },
    with: {
        "componentize-py:test/resource-import-and-export.thing": ThingU32,
        "componentize-py:test/resource-borrow-import.thing": ThingU32,
        "componentize-py:test/resource-with-lists.thing": ThingList,
        "componentize-py:test/resource-aggregates.thing": ThingU32,
        "componentize-py:test/resource-alias1.thing": ThingString,
        "componentize-py:test/resource-floats.float": MyFloat,
        "resource-floats-imports.float": MyFloat,
        "componentize-py:test/resource-borrow-in-record.thing": ThingString,
        "componentize-py:test/host-thing-interface.host-thing": ThingString,
    },
});

mod foo_sdk {
    wasmtime::component::bindgen!({
        path: "src/test/foo_sdk/wit",
        world: "foo-world",
        imports: { default: trappable },
        exports: { default: async },
    });
}

mod bar_sdk {
    wasmtime::component::bindgen!({
        path: "src/test/bar_sdk/wit",
        world: "bar-world",
        imports: { default: async },
        exports: { default: async },
        with: {
            "foo:sdk/foo-interface": super::foo_sdk::foo::sdk::foo_interface,
        },
    });
}

pub struct ThingU32(u32);
pub struct ThingList(Vec<u8>);
pub struct ThingString(String);
pub struct MyFloat(f64);

impl TestsImports for Ctx {
    async fn output(&mut self, _: Frame) -> Result<()> {
        unreachable!()
    }

    async fn get_bytes(&mut self, count: u32) -> Result<Vec<u8>> {
        Ok(vec![42u8; usize::try_from(count).unwrap()])
    }
}

macro_rules! load_guest_code {
    ($($input_string:expr),*) => {
        &[
            $(
                ($input_string, include_str!(concat!("./python_source/", $input_string))),
            )*
        ]
    };
}

const GUEST_CODE: &[(&str, &str)] = load_guest_code!(
    "app.py",
    "resource_import_and_export.py",
    "resource_borrow_export.py",
    "resource_with_lists.py",
    "resource_aggregates.py",
    "resource_alias1.py",
    "resource_floats_exports.py",
    "resource_borrow_in_record.py",
    "streams_and_futures.py"
);

struct Host;

impl super::Host for Host {
    type World = Tests;

    fn add_to_linker(linker: &mut Linker<Ctx>) -> Result<()> {
        wasmtime_wasi::p2::add_to_linker_async(linker)?;
        Tests::add_to_linker::<_, HasSelf<_>>(linker, |ctx| ctx)?;
        foo_sdk::FooWorld::add_to_linker::<_, HasSelf<_>>(linker, |ctx| ctx)?;
        Ok(())
    }

    async fn instantiate_pre(store: &mut Store<Ctx>, pre: InstancePre<Ctx>) -> Result<Self::World> {
        TestsPre::new(pre)?.instantiate_async(store).await
    }
}

struct FooHost;

impl super::Host for FooHost {
    type World = foo_sdk::FooWorld;

    fn add_to_linker(_linker: &mut Linker<Ctx>) -> Result<()> {
        unreachable!()
    }

    async fn instantiate_pre(store: &mut Store<Ctx>, pre: InstancePre<Ctx>) -> Result<Self::World> {
        foo_sdk::FooWorldPre::new(pre)?
            .instantiate_async(store)
            .await
    }
}

struct BarHost;

impl super::Host for BarHost {
    type World = bar_sdk::BarWorld;

    fn add_to_linker(_linker: &mut Linker<Ctx>) -> Result<()> {
        unreachable!()
    }

    async fn instantiate_pre(store: &mut Store<Ctx>, pre: InstancePre<Ctx>) -> Result<Self::World> {
        bar_sdk::BarWorldPre::new(pre)?
            .instantiate_async(store)
            .await
    }
}

static TESTER: Lazy<Tester<Host>> = Lazy::new(|| {
    Tester::<Host>::new(
        include_str!("wit/tests.wit"),
        Some("tests"),
        GUEST_CODE,
        &["src/test"],
        &[("foo_sdk", "foo-world"), ("bar_sdk", "bar-world")],
        *SEED,
    )
    .unwrap()
});

#[test]
fn simple_export() -> Result<()> {
    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 3,
            runtime.block_on(
                world
                    .componentize_py_test_simple_export()
                    .call_foo(store, 42)
            )?
        );

        Ok(())
    })
}

#[test]
fn simple_async_export() -> Result<()> {
    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 3,
            runtime
                .block_on(async {
                    store
                        .run_concurrent(async |store| {
                            world
                                .componentize_py_test_simple_async_export()
                                .call_foo(store, 42)
                                .await
                        })
                        .await?
                })?
                .0
        );

        Ok(())
    })
}

#[test]
fn simple_import_and_export() -> Result<()> {
    impl componentize_py::test::simple_import_and_export::Host for Ctx {
        async fn foo(&mut self, v: u32) -> Result<u32> {
            Ok(v + 2)
        }
    }

    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 2 + 3,
            runtime.block_on(
                world
                    .componentize_py_test_simple_import_and_export()
                    .call_foo(store, 42)
            )?
        );

        Ok(())
    })
}

#[test]
fn simple_async_import_and_export() -> Result<()> {
    impl componentize_py::test::simple_async_import_and_export::Host for Ctx {}

    impl componentize_py::test::simple_async_import_and_export::HostWithStore for HasSelf<Ctx> {
        async fn foo<T>(_: &Accessor<T, Self>, v: u32) -> Result<u32> {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(v + 2)
        }
    }

    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 2 + 3,
            runtime
                .block_on(async {
                    store
                        .run_concurrent(async |store| {
                            world
                                .componentize_py_test_simple_async_import_and_export()
                                .call_foo(store, 42)
                                .await
                        })
                        .await?
                })?
                .0
        );

        Ok(())
    })
}

#[test]
fn resource_import_and_export() -> Result<()> {
    use componentize_py::test::resource_import_and_export::{Host, HostThing};

    impl HostThing for Ctx {
        async fn new(&mut self, v: u32) -> Result<Resource<ThingU32>> {
            Ok(self.ctx().table.push(ThingU32(v + 8))?)
        }

        async fn foo(&mut self, this: Resource<ThingU32>) -> Result<u32> {
            Ok(self.ctx().table.get(&this)?.0 + 1)
        }

        async fn bar(&mut self, this: Resource<ThingU32>, v: u32) -> Result<()> {
            self.ctx().table.get_mut(&this)?.0 = v + 5;
            Ok(())
        }

        async fn baz(
            &mut self,
            a: Resource<ThingU32>,
            b: Resource<ThingU32>,
        ) -> Result<Resource<ThingU32>> {
            let a = self.ctx().table.get(&a)?.0;
            let b = self.ctx().table.get(&b)?.0;

            Ok(self.ctx().table.push(ThingU32(a + b + 6))?)
        }

        async fn drop(&mut self, this: Resource<ThingU32>) -> Result<()> {
            Ok(self.ctx().table.delete(this).map(|_| ())?)
        }
    }

    impl Host for Ctx {}

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_import_and_export();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, 42).await?;

            assert_eq!(
                42 + 7 + 8 + 3 + 1,
                thing.call_foo(&mut *store, thing1).await?
            );

            thing.call_bar(&mut *store, thing1, 33).await?;

            assert_eq!(
                33 + 4 + 5 + 3 + 1,
                thing.call_foo(&mut *store, thing1).await?
            );

            let thing2 = thing.call_constructor(&mut *store, 81).await?;

            let thing3 = thing.call_baz(&mut *store, thing1, thing2).await?;

            assert_eq!(
                33 + 4 + 5 + 81 + 7 + 8 + 6 + 1 + 9 + 7 + 8 + 3 + 1,
                thing.call_foo(&mut *store, thing3).await?
            );

            Ok(())
        })
    })
}

#[test]
fn refcounts() -> Result<()> {
    TESTER.test(|world, store, runtime| runtime.block_on(world.call_test_refcounts(store)))
}

#[test]
fn resource_borrow_import() -> Result<()> {
    use componentize_py::test::resource_borrow_import::{Host, HostThing};

    impl HostThing for Ctx {
        async fn new(&mut self, v: u32) -> Result<Resource<ThingU32>> {
            Ok(self.ctx().table.push(ThingU32(v + 2))?)
        }

        async fn drop(&mut self, this: Resource<ThingU32>) -> Result<()> {
            Ok(self.ctx().table.delete(this).map(|_| ())?)
        }
    }

    impl Host for Ctx {
        async fn foo(&mut self, this: Resource<ThingU32>) -> Result<u32> {
            Ok(self.ctx().table.get(&this)?.0 + 3)
        }
    }

    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 1 + 2 + 3 + 4,
            runtime.block_on(world.call_test_resource_borrow_import(store, 42))?
        );

        Ok(())
    })
}

#[test]
fn resource_borrow_export() -> Result<()> {
    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_borrow_export();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, 42).await?;

            assert_eq!(42 + 1 + 2, instance.call_foo(&mut *store, thing1).await?);

            Ok(())
        })
    })
}

#[test]
fn resource_with_lists() -> Result<()> {
    use componentize_py::test::resource_with_lists::{Host, HostThing};

    impl HostThing for Ctx {
        async fn new(&mut self, mut v: Vec<u8>) -> Result<Resource<ThingList>> {
            v.extend(b" HostThing.new");
            Ok(self.ctx().table.push(ThingList(v))?)
        }

        async fn foo(&mut self, this: Resource<ThingList>) -> Result<Vec<u8>> {
            let mut v = self.ctx().table.get(&this)?.0.clone();
            v.extend(b" HostThing.foo");
            Ok(v)
        }

        async fn bar(&mut self, this: Resource<ThingList>, mut v: Vec<u8>) -> Result<()> {
            v.extend(b" HostThing.bar");
            self.ctx().table.get_mut(&this)?.0 = v;
            Ok(())
        }

        async fn baz(&mut self, mut v: Vec<u8>) -> Result<Vec<u8>> {
            v.extend(b" HostThing.baz");
            Ok(v)
        }

        async fn drop(&mut self, this: Resource<ThingList>) -> Result<()> {
            Ok(self.ctx().table.delete(this).map(|_| ())?)
        }
    }

    impl Host for Ctx {}

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_with_lists();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, b"Hi").await?;

            assert_eq!(
                "Hi Thing.__init__ HostThing.new HostThing.foo Thing.foo",
                str::from_utf8(&thing.call_foo(&mut *store, thing1).await?)?
            );

            thing.call_bar(&mut *store, thing1, b"Hola").await?;

            assert_eq!(
                "Hola Thing.bar HostThing.bar HostThing.foo Thing.foo",
                str::from_utf8(&thing.call_foo(&mut *store, thing1).await?)?
            );

            assert_eq!(
                "Ohayo Gozaimas Thing.baz HostThing.baz Thing.baz again",
                str::from_utf8(&thing.call_baz(&mut *store, b"Ohayo Gozaimas").await?)?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_aggregates() -> Result<()> {
    {
        use componentize_py::test::resource_aggregates::{
            Host, HostThing, L1, L2, R1, R2, R3, T1, T2, V1, V2,
        };

        impl HostThing for Ctx {
            async fn new(&mut self, v: u32) -> Result<Resource<ThingU32>> {
                Ok(self.ctx().table.push(ThingU32(v + 2))?)
            }

            async fn drop(&mut self, this: Resource<ThingU32>) -> Result<()> {
                Ok(self.ctx().table.delete(this).map(|_| ())?)
            }
        }

        impl Host for Ctx {
            async fn foo(
                &mut self,
                r1: R1,
                r2: R2,
                r3: R3,
                t1: T1,
                t2: T2,
                v1: V1,
                v2: V2,
                l1: L1,
                l2: L2,
                o1: Option<Resource<ThingU32>>,
                o2: Option<Resource<ThingU32>>,
                result1: Result<Resource<ThingU32>, ()>,
                result2: Result<Resource<ThingU32>, ()>,
            ) -> Result<u32> {
                let V1::Thing(v1) = v1;
                let V2::Thing(v2) = v2;
                Ok(self.ctx().table.get(&r1.thing)?.0
                    + self.ctx().table.get(&r2.thing)?.0
                    + self.ctx().table.get(&r3.thing1)?.0
                    + self.ctx().table.get(&r3.thing2)?.0
                    + self.ctx().table.get(&t1.0)?.0
                    + self.ctx().table.get(&t1.1.thing)?.0
                    + self.ctx().table.get(&t2.0)?.0
                    + self.ctx().table.get(&v1)?.0
                    + self.ctx().table.get(&v2)?.0
                    + l1.into_iter()
                        .try_fold(0, |n, v| Ok::<_, Error>(self.ctx().table.get(&v)?.0 + n))?
                    + l2.into_iter()
                        .try_fold(0, |n, v| Ok::<_, Error>(self.ctx().table.get(&v)?.0 + n))?
                    + o1.map(|v| Ok::<_, Error>(self.ctx().table.get(&v)?.0))
                        .unwrap_or(Ok(0))?
                    + o2.map(|v| Ok::<_, Error>(self.ctx().table.get(&v)?.0))
                        .unwrap_or(Ok(0))?
                    + result1
                        .map(|v| Ok::<_, Error>(self.ctx().table.get(&v)?.0))
                        .unwrap_or(Ok(0))?
                    + result2
                        .map(|v| Ok::<_, Error>(self.ctx().table.get(&v)?.0))
                        .unwrap_or(Ok(0))?
                    + 3)
            }
        }
    }

    use exports::componentize_py::test::resource_aggregates::{R1, R2, R3, V1, V2};

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_aggregates();
            let thing = instance.thing();

            let mut things = Vec::new();
            for n in 1..18 {
                things.push(thing.call_constructor(&mut *store, n).await?);
            }

            assert_eq!(
                (1..18).map(|n| n + 1 + 2).sum::<u32>() + 3 + 4,
                instance
                    .call_foo(
                        &mut *store,
                        R1 { thing: things[0] },
                        R2 { thing: things[1] },
                        R3 {
                            thing1: things[2],
                            thing2: things[3]
                        },
                        (things[4], R1 { thing: things[5] }),
                        (things[6],),
                        V1::Thing(things[7]),
                        V2::Thing(things[8]),
                        &vec![things[9], things[10]],
                        &vec![things[11], things[12]],
                        Some(things[13]),
                        Some(things[14]),
                        Ok(things[15]),
                        Ok(things[16])
                    )
                    .await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_alias() -> Result<()> {
    {
        use componentize_py::test::resource_alias1::{Foo, Host, HostThing};

        impl HostThing for Ctx {
            async fn new(&mut self, s: String) -> Result<Resource<ThingString>> {
                Ok(self.ctx().table.push(ThingString(s + " HostThing::new"))?)
            }

            async fn get(&mut self, this: Resource<ThingString>) -> Result<String> {
                Ok(format!("{} HostThing.get", self.ctx().table.get(&this)?.0))
            }

            async fn drop(&mut self, this: Resource<ThingString>) -> Result<()> {
                Ok(self.ctx().table.delete(this).map(|_| ())?)
            }
        }

        impl Host for Ctx {
            async fn a(&mut self, f: Foo) -> Result<Vec<Resource<ThingString>>> {
                Ok(vec![f.thing])
            }
        }
    }

    {
        use componentize_py::test::resource_alias2::{Bar, Foo, Host, Thing};

        impl Host for Ctx {
            async fn b(&mut self, f: Foo, g: Bar) -> Result<Vec<Resource<Thing>>> {
                Ok(vec![f.thing, g.thing])
            }
        }
    }

    use exports::componentize_py::test::{resource_alias1, resource_alias2};

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let thing1 = store
                .data_mut()
                .ctx()
                .table
                .push(ThingString("Ni Hao".to_string()))?;

            fn host_things_to_strings(
                store: &mut Store<Ctx>,
                things: Vec<Resource<ThingString>>,
            ) -> Result<Vec<String>> {
                let mut strings = Vec::new();
                for thing in things {
                    strings.push(store.data_mut().ctx().table.get(&thing)?.0.clone());
                }

                Ok(strings)
            }

            let things = world
                .call_test_resource_alias(&mut *store, &[thing1])
                .await?;

            assert_eq!(
                vec!["Ni Hao".to_string()],
                host_things_to_strings(&mut *store, things)?
            );

            let instance = world.componentize_py_test_resource_alias1();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, "Ciao").await?;

            async fn guest_things_to_strings(
                store: &mut Store<Ctx>,
                thing: &resource_alias1::GuestThing<'_>,
                things: Vec<ResourceAny>,
            ) -> Result<Vec<String>> {
                let mut strings = Vec::new();
                for t in things {
                    strings.push(thing.call_get(&mut *store, t).await?);
                }

                Ok(strings)
            }

            let things = instance
                .call_a(&mut *store, resource_alias1::Foo { thing: thing1 })
                .await?;

            assert_eq!(
                vec!["Ciao Thing.__init__ HostThing::new HostThing.get Thing.get".to_string()],
                guest_things_to_strings(&mut *store, &thing, things).await?
            );

            let instance = world.componentize_py_test_resource_alias2();
            let thing1 = thing.call_constructor(&mut *store, "Ciao").await?;
            let thing2 = thing.call_constructor(&mut *store, "Aloha").await?;

            let things = instance
                .call_b(
                    &mut *store,
                    resource_alias2::Foo { thing: thing1 },
                    resource_alias2::Bar { thing: thing2 },
                )
                .await?;

            assert_eq!(
                vec![
                    "Ciao Thing.__init__ HostThing::new HostThing.get Thing.get".to_string(),
                    "Aloha Thing.__init__ HostThing::new HostThing.get Thing.get".to_string()
                ],
                guest_things_to_strings(&mut *store, &thing, things).await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_floats() -> Result<()> {
    {
        use resource_floats_imports::{Host, HostFloat};

        impl HostFloat for Ctx {
            async fn new(&mut self, v: f64) -> Result<Resource<MyFloat>> {
                Ok(self.ctx().table.push(MyFloat(v + 2_f64))?)
            }

            async fn get(&mut self, this: Resource<MyFloat>) -> Result<f64> {
                Ok(self.ctx().table.get(&this)?.0 + 4_f64)
            }

            async fn add(&mut self, a: Resource<MyFloat>, b: f64) -> Result<Resource<MyFloat>> {
                let a = self.ctx().table.get(&a)?.0;
                Ok(self.ctx().table.push(MyFloat(a + b + 6_f64))?)
            }

            async fn drop(&mut self, this: Resource<MyFloat>) -> Result<()> {
                Ok(self.ctx().table.delete(this).map(|_| ())?)
            }
        }

        impl Host for Ctx {}
    }

    {
        use componentize_py::test::resource_floats::{Host, HostFloat};

        impl HostFloat for Ctx {
            async fn new(&mut self, v: f64) -> Result<Resource<MyFloat>> {
                Ok(self.ctx().table.push(MyFloat(v + 1_f64))?)
            }

            async fn get(&mut self, this: Resource<MyFloat>) -> Result<f64> {
                Ok(self.ctx().table.get(&this)?.0 + 3_f64)
            }

            async fn drop(&mut self, this: Resource<MyFloat>) -> Result<()> {
                Ok(self.ctx().table.delete(this).map(|_| ())?)
            }
        }

        impl Host for Ctx {}
    }

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let float1 = store.data_mut().ctx().table.push(MyFloat(42_f64))?;
            let float2 = store.data_mut().ctx().table.push(MyFloat(55_f64))?;
            let sum = world.call_add(&mut *store, float1, float2).await?;

            assert_eq!(
                42_f64 + 3_f64 + 55_f64 + 3_f64 + 5_f64 + 1_f64,
                store.data_mut().ctx().table.get(&sum)?.0
            );

            let instance = world.resource_floats_exports();
            let float = instance.float();
            let float1 = float.call_constructor(&mut *store, 22_f64).await?;

            assert_eq!(
                22_f64 + 1_f64 + 2_f64 + 4_f64 + 3_f64,
                float.call_get(&mut *store, float1).await?
            );

            let result = float.call_add(&mut *store, float1, 7_f64).await?;

            assert_eq!(
                22_f64
                    + 1_f64
                    + 2_f64
                    + 7_f64
                    + 6_f64
                    + 4_f64
                    + 5_f64
                    + 1_f64
                    + 2_f64
                    + 4_f64
                    + 3_f64,
                float.call_get(&mut *store, result).await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_borrow_in_record() -> Result<()> {
    {
        use componentize_py::test::resource_borrow_in_record::{Foo, Host, HostThing};

        impl HostThing for Ctx {
            async fn new(&mut self, v: String) -> Result<Resource<ThingString>> {
                Ok(self.ctx().table.push(ThingString(v + " HostThing::new"))?)
            }

            async fn get(&mut self, this: Resource<ThingString>) -> Result<String> {
                Ok(format!("{} HostThing.get", self.ctx().table.get(&this)?.0))
            }

            async fn drop(&mut self, this: Resource<ThingString>) -> Result<()> {
                Ok(self.ctx().table.delete(this).map(|_| ())?)
            }
        }

        impl Host for Ctx {
            async fn test(&mut self, list: Vec<Foo>) -> Result<Vec<Resource<ThingString>>> {
                list.into_iter()
                    .map(|foo| {
                        let value = self.ctx().table.get(&foo.thing)?.0.clone();
                        Ok(self
                            .ctx()
                            .table
                            .push(ThingString(value + " HostThing::test"))?)
                    })
                    .collect()
            }
        }
    }

    use exports::componentize_py::test::resource_borrow_in_record::{Foo, GuestThing};

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_borrow_in_record();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, "Bonjour").await?;
            let thing2 = thing.call_constructor(&mut *store, "mon cher").await?;

            let things = instance
                .call_test(&mut *store, &[Foo { thing: thing1 }, Foo { thing: thing2 }])
                .await?;

            async fn things_to_strings(
                store: &mut Store<Ctx>,
                thing: &GuestThing<'_>,
                things: Vec<ResourceAny>,
            ) -> Result<Vec<String>> {
                let mut strings = Vec::new();
                for t in things {
                    strings.push(thing.call_get(&mut *store, t).await?);
                }

                Ok(strings)
            }

            assert_eq!(
                vec![
                    "Bonjour Thing.__init__ HostThing::new \
                     HostThing::test HostThing.get Thing.get"
                        .to_string(),
                    "mon cher Thing.__init__ HostThing::new \
                     HostThing::test HostThing.get Thing.get"
                        .to_string()
                ],
                things_to_strings(&mut *store, &thing, things).await?
            );

            Ok(())
        })
    })
}

#[test]
fn multiworld() -> Result<()> {
    impl foo_sdk::foo::sdk::foo_interface::Host for Ctx {
        fn test(&mut self, s: String) -> Result<String> {
            Ok(format!("{s} HostFoo::test"))
        }
    }

    TESTER.test_with::<FooHost>(|world, store, runtime| {
        runtime.block_on(async {
            let result = world
                .foo_sdk_foo_interface()
                .call_test(store, "Howdy")
                .await?;

            assert_eq!("Howdy FooInterface.test HostFoo::test", result);

            Ok(())
        })
    })?;

    TESTER.test_with::<BarHost>(|world, store, runtime| {
        runtime.block_on(async {
            let result = world
                .bar_sdk_bar_interface()
                .call_test(store, "Howdy")
                .await?;

            assert_eq!("Howdy BarInterface.test HostFoo::test", result);

            Ok(())
        })
    })
}

#[test]
fn filesystem() -> Result<()> {
    let filename = "foo.txt";
    let message = b"The Jabberwock, with eyes of flame";

    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join(filename), message)?;
    let wasi = WasiCtxBuilder::new()
        .inherit_stdout()
        .inherit_stderr()
        .preopened_dir(dir.path(), "/", DirPerms::all(), FilePerms::all())?
        .build();

    TESTER.test_with_wasi::<Host>(wasi, |world, store, runtime| {
        runtime.block_on(async {
            let value = world
                .call_read_file(store, filename)
                .await?
                .map_err(|s| anyhow!("{s}"))?;

            assert_eq!(&value, message);

            Ok(())
        })
    })
}

struct VecProducer<T> {
    source: Vec<T>,
    sleep: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl<T> VecProducer<T> {
    fn new(source: Vec<T>, delay: bool) -> Self {
        Self {
            source,
            sleep: if delay {
                tokio::time::sleep(Duration::from_millis(10)).boxed()
            } else {
                async {}.boxed()
            },
        }
    }
}

impl<D, T: Lift + Unpin + 'static> StreamProducer<D> for VecProducer<T> {
    type Item = T;
    type Buffer = VecBuffer<T>;

    fn poll_produce(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        _: StoreContextMut<D>,
        mut destination: Destination<Self::Item, Self::Buffer>,
        _: bool,
    ) -> Poll<Result<StreamResult>> {
        let sleep = &mut self.as_mut().get_mut().sleep;
        task::ready!(sleep.as_mut().poll(cx));
        *sleep = async {}.boxed();

        destination.set_buffer(mem::take(&mut self.get_mut().source).into());
        Poll::Ready(Ok(StreamResult::Dropped))
    }
}

struct VecConsumer<T> {
    destination: Arc<Mutex<Vec<T>>>,
    sleep: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl<T> VecConsumer<T> {
    fn new(destination: Arc<Mutex<Vec<T>>>, delay: bool) -> Self {
        Self {
            destination,
            sleep: if delay {
                tokio::time::sleep(Duration::from_millis(10)).boxed()
            } else {
                async {}.boxed()
            },
        }
    }
}

impl<D, T: Lift + 'static> StreamConsumer<D> for VecConsumer<T> {
    type Item = T;

    fn poll_consume(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        store: StoreContextMut<D>,
        mut source: Source<Self::Item>,
        _: bool,
    ) -> Poll<Result<StreamResult>> {
        let sleep = &mut self.as_mut().get_mut().sleep;
        task::ready!(sleep.as_mut().poll(cx));
        *sleep = async {}.boxed();

        source.read(store, self.destination.lock().unwrap().deref_mut())?;
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

#[test]
fn echo_stream_u8() -> Result<()> {
    test_echo_stream_u8(false)
}

#[test]
fn echo_stream_u8_with_delay() -> Result<()> {
    test_echo_stream_u8(true)
}

fn test_echo_stream_u8(delay: bool) -> Result<()> {
    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            store
                .run_concurrent(async |store| {
                    let expected =
                        b"Beware the Jubjub bird, and shun\n\tThe frumious Bandersnatch!";
                    let stream = store.with(|store| {
                        StreamReader::new(store, VecProducer::new(expected.to_vec(), delay))
                    });

                    let (stream, task) = world
                        .componentize_py_test_streams_and_futures()
                        .call_echo_stream_u8(store, stream)
                        .await?;

                    let received = Arc::new(Mutex::new(Vec::with_capacity(expected.len())));
                    store.with(|store| {
                        stream.pipe(store, VecConsumer::new(received.clone(), delay))
                    });

                    task.block(store).await;

                    assert_eq!(expected, &received.lock().unwrap()[..]);

                    anyhow::Ok(())
                })
                .await?
        })?;

        Ok(())
    })
}

struct OptionProducer<T> {
    source: Option<T>,
    sleep: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl<T> OptionProducer<T> {
    fn new(source: Option<T>, delay: bool) -> Self {
        Self {
            source,
            sleep: if delay {
                tokio::time::sleep(Duration::from_millis(10)).boxed()
            } else {
                async {}.boxed()
            },
        }
    }
}

impl<D, T: Unpin + Send + 'static> FutureProducer<D> for OptionProducer<T> {
    type Item = T;

    fn poll_produce(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        _: StoreContextMut<D>,
        _: bool,
    ) -> Poll<Result<Option<T>>> {
        let sleep = &mut self.as_mut().get_mut().sleep;
        task::ready!(sleep.as_mut().poll(cx));
        *sleep = async {}.boxed();

        Poll::Ready(Ok(self.get_mut().source.take()))
    }
}

struct OptionConsumer<T> {
    destination: Arc<Mutex<Option<T>>>,
    sleep: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl<T> OptionConsumer<T> {
    fn new(destination: Arc<Mutex<Option<T>>>, delay: bool) -> Self {
        Self {
            destination,
            sleep: if delay {
                tokio::time::sleep(Duration::from_millis(10)).boxed()
            } else {
                async {}.boxed()
            },
        }
    }
}

impl<D, T: Lift + 'static> FutureConsumer<D> for OptionConsumer<T> {
    type Item = T;

    fn poll_consume(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        store: StoreContextMut<D>,
        mut source: Source<Self::Item>,
        _: bool,
    ) -> Poll<Result<()>> {
        let sleep = &mut self.as_mut().get_mut().sleep;
        task::ready!(sleep.as_mut().poll(cx));
        *sleep = async {}.boxed();

        source.read(store, self.destination.lock().unwrap().deref_mut())?;
        Poll::Ready(Ok(()))
    }
}

#[test]
fn echo_future_string() -> Result<()> {
    test_echo_future_string(false)
}

#[test]
fn echo_future_string_with_delay() -> Result<()> {
    test_echo_future_string(true)
}

fn test_echo_future_string(delay: bool) -> Result<()> {
    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            store
                .run_concurrent(async |store| {
                    let expected = "Beware the Jubjub bird, and shun\n\tThe frumious Bandersnatch!";
                    let future = store.with(|store| {
                        FutureReader::new(
                            store,
                            OptionProducer::new(Some(expected.to_string()), delay),
                        )
                    });

                    let (future, task) = world
                        .componentize_py_test_streams_and_futures()
                        .call_echo_future_string(store, future)
                        .await?;

                    let received = Arc::new(Mutex::new(None::<String>));
                    store.with(|store| {
                        future.pipe(store, OptionConsumer::new(received.clone(), delay))
                    });

                    task.block(store).await;

                    assert_eq!(
                        expected,
                        received.lock().unwrap().as_ref().unwrap().as_str()
                    );

                    anyhow::Ok(())
                })
                .await?
        })?;

        Ok(())
    })
}

struct OneAtATime<T> {
    destination: Arc<Mutex<Vec<T>>>,
    sleep: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl<T> OneAtATime<T> {
    fn new(destination: Arc<Mutex<Vec<T>>>, delay: bool) -> Self {
        Self {
            destination,
            sleep: if delay {
                tokio::time::sleep(Duration::from_millis(10)).boxed()
            } else {
                async {}.boxed()
            },
        }
    }
}

impl<D, T: Lift + 'static> StreamConsumer<D> for OneAtATime<T> {
    type Item = T;

    fn poll_consume(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        store: StoreContextMut<D>,
        mut source: Source<Self::Item>,
        _: bool,
    ) -> Poll<Result<StreamResult>> {
        let sleep = &mut self.as_mut().get_mut().sleep;
        task::ready!(sleep.as_mut().poll(cx));
        *sleep = async {}.boxed();

        let value = &mut None;
        source.read(store, value)?;
        self.destination.lock().unwrap().push(value.take().unwrap());
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

#[test]
fn short_reads() -> Result<()> {
    test_short_reads(false)
}

#[test]
fn short_reads_with_delay() -> Result<()> {
    test_short_reads(true)
}

fn test_short_reads(delay: bool) -> Result<()> {
    TESTER.test(|world, mut store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_streams_and_futures();
            let thing = instance.thing();

            let strings = ["a", "b", "c", "d", "e"];
            let mut things = Vec::with_capacity(strings.len());
            for string in strings {
                things.push(thing.call_constructor(&mut store, string).await?);
            }

            let received_things = store
                .run_concurrent(async |store| {
                    let count = things.len();
                    // Write the items all at once.  The receiver will only read them
                    // one at a time, forcing us to retake ownership of the unwritten
                    // items between writes.
                    let stream = store
                        .with(|store| StreamReader::new(store, VecProducer::new(things, delay)));

                    let (stream, task) = instance.call_short_reads(store, stream).await?;

                    let received_things = Arc::new(Mutex::new(
                        Vec::<streams_and_futures::Thing>::with_capacity(count),
                    ));
                    // Read only one item at a time, forcing the sender to retake
                    // ownership of any unwritten items.
                    store.with(|store| {
                        stream.pipe(store, OneAtATime::new(received_things.clone(), delay))
                    });

                    task.block(store).await;

                    assert_eq!(count, received_things.lock().unwrap().len());

                    let mut received_strings = Vec::with_capacity(strings.len());
                    let received_things = mem::take(received_things.lock().unwrap().deref_mut());
                    for &it in &received_things {
                        received_strings.push(thing.call_get(store, it).await?.0);
                    }

                    assert_eq!(
                        &strings[..],
                        &received_strings
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                    );

                    anyhow::Ok(received_things)
                })
                .await??;

            for it in received_things {
                it.resource_drop_async::<()>(&mut store).await?;
            }

            anyhow::Ok(())
        })?;

        Ok(())
    })
}

#[test]
fn short_reads_host() -> Result<()> {
    test_short_reads_host(false)
}

#[test]
fn short_reads_host_with_delay() -> Result<()> {
    test_short_reads_host(true)
}

fn test_short_reads_host(delay: bool) -> Result<()> {
    use componentize_py::test::host_thing_interface::{
        Host, HostHostThing, HostHostThingWithStore,
    };

    impl HostHostThingWithStore for HasSelf<Ctx> {
        async fn get<T>(
            accessor: &Accessor<T, Self>,
            this: Resource<ThingString>,
        ) -> Result<String> {
            accessor.with(|mut store| Ok(store.get().table.get(&this)?.0.clone()))
        }
    }

    impl HostHostThing for Ctx {
        async fn new(&mut self, v: String) -> Result<Resource<ThingString>> {
            Ok(self.ctx().table.push(ThingString(v))?)
        }

        async fn drop(&mut self, this: Resource<ThingString>) -> Result<()> {
            Ok(self.ctx().table.delete(this).map(|_| ())?)
        }
    }

    impl Host for Ctx {}

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_streams_and_futures();

            let strings = ["a", "b", "c", "d", "e"];
            let mut things = Vec::with_capacity(strings.len());
            for string in strings {
                things.push(store.data_mut().table.push(ThingString(string.into()))?);
            }

            store
                .run_concurrent(async |store| {
                    let count = things.len();
                    // Write the items all at once.  The receiver will only read them
                    // one at a time, forcing us to retake ownership of the unwritten
                    // items between writes.
                    let stream = store
                        .with(|store| StreamReader::new(store, VecProducer::new(things, delay)));

                    let (stream, task) = instance.call_short_reads_host(store, stream).await?;

                    let received_things = Arc::new(Mutex::new(
                        Vec::<Resource<ThingString>>::with_capacity(count),
                    ));
                    // Read only one item at a time, forcing the sender to retake
                    // ownership of any unwritten items.
                    store.with(|store| {
                        stream.pipe(store, OneAtATime::new(received_things.clone(), delay))
                    });

                    task.block(store).await;

                    assert_eq!(count, received_things.lock().unwrap().len());

                    let received_strings = store.with(|mut store| {
                        mem::take(received_things.lock().unwrap().deref_mut())
                            .into_iter()
                            .map(|v| Ok(store.get().table.delete(v)?.0))
                            .collect::<Result<Vec<_>>>()
                    })?;

                    assert_eq!(
                        &strings[..],
                        &received_strings
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                    );

                    anyhow::Ok(())
                })
                .await?
        })?;

        Ok(())
    })
}

#[test]
fn dropped_future_reader() -> Result<()> {
    test_dropped_future_reader(false)
}

#[test]
fn dropped_future_reader_with_delay() -> Result<()> {
    test_dropped_future_reader(true)
}

fn test_dropped_future_reader(delay: bool) -> Result<()> {
    TESTER.test(|world, mut store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_streams_and_futures();
            let thing = instance.thing();

            let it = store
                .run_concurrent(async |store| {
                    let expected = "Beware the Jubjub bird, and shun\n\tThe frumious Bandersnatch!";
                    let ((mut rx1, rx2), task) = instance
                        .call_dropped_future_reader(store, expected.into())
                        .await?;
                    // Close the future without reading the value.  This will
                    // force the sender to retake ownership of the value it
                    // tried to write.
                    rx1.close_with(store);

                    let received = Arc::new(Mutex::new(None::<streams_and_futures::Thing>));
                    store.with(|store| {
                        rx2.pipe(store, OptionConsumer::new(received.clone(), delay))
                    });

                    task.block(store).await;

                    let it = received.lock().unwrap().take().unwrap();

                    assert_eq!(expected, &thing.call_get(store, it).await?.0);

                    anyhow::Ok(it)
                })
                .await??;

            it.resource_drop_async::<()>(&mut store).await?;

            anyhow::Ok(())
        })?;

        Ok(())
    })
}

#[test]
fn dropped_future_reader_host() -> Result<()> {
    test_dropped_future_reader_host(false)
}

#[test]
fn dropped_future_reader_host_with_delay() -> Result<()> {
    test_dropped_future_reader_host(true)
}

fn test_dropped_future_reader_host(delay: bool) -> Result<()> {
    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_streams_and_futures();

            store
                .run_concurrent(async |store| {
                    let expected = "Beware the Jubjub bird, and shun\n\tThe frumious Bandersnatch!";
                    let ((mut rx1, rx2), task) = instance
                        .call_dropped_future_reader_host(store, expected.into())
                        .await?;
                    // Close the future without reading the value.  This will
                    // force the sender to retake ownership of the value it
                    // tried to write.
                    rx1.close_with(store);

                    let received = Arc::new(Mutex::new(None::<Resource<ThingString>>));
                    store.with(|store| {
                        rx2.pipe(store, OptionConsumer::new(received.clone(), delay))
                    });

                    task.block(store).await;

                    let it = store.with(|mut store| {
                        anyhow::Ok(
                            store
                                .get()
                                .table
                                .delete(received.lock().unwrap().take().unwrap())?
                                .0,
                        )
                    })?;

                    assert_eq!(expected, &it);

                    anyhow::Ok(it)
                })
                .await?
        })?;

        Ok(())
    })
}
